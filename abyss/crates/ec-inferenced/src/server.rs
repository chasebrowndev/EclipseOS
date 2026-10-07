// SPDX-License-Identifier: AGPL-3.0-only
//! Connection handling: one reader thread per connection, one thread per
//! in-flight `Complete`, replies serialised through a per-connection writer.
//!
//! A model call takes seconds to minutes, so it never runs on the reader:
//! the connection keeps accepting requests (including a `Close`) while it
//! waits. In-flight work is capped across all connections; beyond the cap a
//! request is answered `rate_limited` at once rather than queued, so a
//! runaway agent cannot pile up threads.

use crate::api::failure;
use crate::router::Router;
use ec_inference_wire::{read_frame, write_frame, Reply, Request, ToRouter};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub const MAX_IN_FLIGHT: usize = 16;

pub struct Server {
    router: Router,
    in_flight: AtomicUsize,
    max_in_flight: usize,
}

/// Releases an in-flight slot on drop, including on a panic.
struct Slot(Arc<Server>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Server {
    pub fn new(router: Router, max_in_flight: usize) -> Arc<Server> {
        Arc::new(Server {
            router,
            in_flight: AtomicUsize::new(0),
            max_in_flight,
        })
    }

    fn acquire(self: &Arc<Self>) -> Option<Slot> {
        let prev = self.in_flight.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max_in_flight {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(self.clone()))
    }
}

type Writer = Arc<Mutex<UnixStream>>;

fn send(w: &Writer, reply: &Reply) {
    // A poisoned lock means another reply thread panicked mid-write; the
    // stream may hold half a frame, so stop writing to it.
    let Ok(mut s) = w.lock() else { return };
    // A write error means the peer is gone; the reader will see EOF.
    let _ = write_frame(&mut *s, &reply.to_json());
}

/// One log line per request: never content, never the key.
fn log(req: &Request, outcome: &str, input: u64, output: u64) {
    eprintln!(
        "ec-inferenced: task={} package={} model={} outcome={} input_tokens={} output_tokens={}",
        req.task, req.package, req.model, outcome, input, output
    );
}

/// Serve one accepted connection until it closes. Blocks; call it on the
/// connection's own thread.
pub fn serve_conn(server: Arc<Server>, stream: UnixStream) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(write_half));
    let mut reader = stream;
    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(v)) => v,
            // EOF, an oversized frame or bad JSON: the connection is done.
            Ok(None) | Err(_) => return,
        };
        match ToRouter::from_json(&frame) {
            Some(ToRouter::Close { task }) => server.router.close(&task),
            Some(ToRouter::Complete(req)) => dispatch(&server, &writer, req),
            None => {
                // Answer a request we can identify; drop one we cannot.
                if let Some(id) = frame.get("id").and_then(|v| v.as_u64()) {
                    send(
                        &writer,
                        &Reply {
                            id,
                            result: Err(failure("bad_request", "malformed request")),
                        },
                    );
                }
            }
        }
    }
}

fn dispatch(server: &Arc<Server>, writer: &Writer, req: Request) {
    let Some(slot) = server.acquire() else {
        log(&req, "rate_limited", 0, 0);
        send(
            writer,
            &Reply {
                id: req.id,
                result: Err(failure("rate_limited", "too many inference requests in flight")),
            },
        );
        return;
    };
    let (server, writer) = (server.clone(), writer.clone());
    let spawned = std::thread::Builder::new()
        .name("inference".into())
        .spawn(move || {
            let _slot = slot;
            let result = server.router.complete(&req);
            match &result {
                Ok(c) => log(&req, "ok", c.input_tokens, c.output_tokens),
                Err(f) => log(&req, &f.kind, 0, 0),
            }
            send(&writer, &Reply { id: req.id, result });
        });
    // Thread creation failed: the closure (and its slot) is dropped with the
    // error, and the request gets no reply; agentd's own timeout reports it.
    if let Err(e) = spawned {
        eprintln!("ec-inferenced: cannot spawn a worker: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Http, HttpError, HttpResponse};
    use crate::credential::Credentials;
    use crate::secret::ApiKey;
    use ec_inference_wire::{Backend, Failure, Value};
    use serde_json::json;
    use std::sync::mpsc;

    /// Fake network: replies with a canned completion, and can hold the call
    /// named by `hold_model` until released.
    struct FakeHttp {
        seen: Mutex<Vec<(String, Value)>>,
        hold_model: Option<String>,
        release: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl FakeHttp {
        fn new() -> Arc<FakeHttp> {
            Arc::new(FakeHttp {
                seen: Mutex::new(Vec::new()),
                hold_model: None,
                release: Mutex::new(None),
            })
        }
    }

    impl Http for FakeHttp {
        fn post_messages(&self, key: &ApiKey, body: &[u8]) -> Result<HttpResponse, HttpError> {
            let v: Value = serde_json::from_slice(body).unwrap();
            let model = v["model"].as_str().unwrap().to_owned();
            self.seen.lock().unwrap().push((key.expose().to_owned(), v));
            if self.hold_model.as_deref() == Some(&model) {
                if let Some(rx) = self.release.lock().unwrap().take() {
                    let _ = rx.recv();
                }
            }
            let body = json!({
                "model": model,
                "content": [{"type": "text", "text": "hello"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 5, "output_tokens": 2},
            });
            Ok(HttpResponse {
                status: 200,
                body: body.to_string().into_bytes(),
            })
        }
    }

    struct FakeCreds(Result<&'static str, (&'static str, &'static str)>);
    impl Credentials for FakeCreds {
        fn api_key(&self, package: &str) -> Result<ApiKey, Failure> {
            assert_eq!(package, "pkg");
            match self.0 {
                Ok(k) => Ok(ApiKey::from_bytes(k.as_bytes()).unwrap()),
                Err((kind, m)) => Err(failure(kind, m)),
            }
        }
    }

    fn request(id: u64, backend: Backend, model: &str) -> ToRouter {
        ToRouter::Complete(Request {
            id,
            task: "T".into(),
            package: "pkg".into(),
            backend,
            model: model.into(),
            system: String::new(),
            messages: json!([{"role": "user", "content": "hi"}]),
            tools: json!([]),
            max_tokens: 100,
        })
    }

    fn start(http: Arc<FakeHttp>, creds: FakeCreds, max: usize) -> UnixStream {
        let router = Router::new(http, Arc::new(creds));
        let server = Server::new(router, max);
        let (a, b) = UnixStream::pair().unwrap();
        std::thread::spawn(move || serve_conn(server, b));
        a
    }

    fn call(c: &mut UnixStream, m: &ToRouter) {
        write_frame(c, &m.to_json()).unwrap();
    }

    fn reply(c: &mut UnixStream) -> Reply {
        Reply::from_json(&read_frame(c).unwrap().unwrap()).unwrap()
    }

    #[test]
    fn a_completion_round_trips_and_the_key_reaches_only_the_transport() {
        let http = FakeHttp::new();
        let mut c = start(http.clone(), FakeCreds(Ok("sk-ant-KEY")), 4);
        call(&mut c, &request(7, Backend::Api, "claude-x"));
        let r = reply(&mut c);
        assert_eq!(r.id, 7);
        let ok = r.result.unwrap();
        assert_eq!(ok.stop_reason, "end_turn");
        assert_eq!((ok.input_tokens, ok.output_tokens), (5, 2));
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].0, "sk-ant-KEY");
        assert_eq!(seen[0].1["fallbacks"], "default");
    }

    #[test]
    fn credential_failures_pass_through() {
        let mut c = start(
            FakeHttp::new(),
            FakeCreds(Err((
                "broker_locked",
                "brokerd is locked: run `ec-secret unlock`",
            ))),
            4,
        );
        call(&mut c, &request(1, Backend::Api, "m"));
        let f = reply(&mut c).result.unwrap_err();
        assert_eq!(f.kind, "broker_locked");
    }

    #[test]
    fn claude_code_is_unavailable_and_close_is_accepted() {
        let mut c = start(FakeHttp::new(), FakeCreds(Ok("sk-x")), 4);
        call(&mut c, &ToRouter::Close { task: "T".into() });
        call(&mut c, &request(2, Backend::ClaudeCode, "m"));
        let r = reply(&mut c);
        assert_eq!(r.id, 2);
        let f = r.result.unwrap_err();
        assert_eq!(f.kind, "backend_unavailable");
        assert_eq!(f.message, "the Claude Code backend is not built yet");
    }

    #[test]
    fn a_slow_call_does_not_block_another_and_the_cap_answers_rate_limited() {
        let (release_tx, release_rx) = mpsc::channel();
        let http = Arc::new(FakeHttp {
            seen: Mutex::new(Vec::new()),
            hold_model: Some("slow".into()),
            release: Mutex::new(Some(release_rx)),
        });
        let mut c = start(http, FakeCreds(Ok("sk-x")), 1);
        call(&mut c, &request(1, Backend::Api, "slow"));
        // Wait until the slow call occupies the one slot: a second request is
        // refused immediately (the reply order proves nothing was queued).
        let mut limited = None;
        for i in 0..200u64 {
            call(&mut c, &request(100 + i, Backend::Api, "fast"));
            let r = reply(&mut c);
            if let Err(f) = &r.result {
                limited = Some(f.kind.clone());
                break;
            }
            // The slow call had not started yet and this one took the slot
            // and finished: try again.
        }
        assert_eq!(limited.as_deref(), Some("rate_limited"));
        release_tx.send(()).unwrap();
        let r = reply(&mut c);
        assert_eq!(r.id, 1);
        assert!(r.result.is_ok());
    }

    #[test]
    fn two_calls_overlap() {
        let (release_tx, release_rx) = mpsc::channel();
        let http = Arc::new(FakeHttp {
            seen: Mutex::new(Vec::new()),
            hold_model: Some("slow".into()),
            release: Mutex::new(Some(release_rx)),
        });
        let mut c = start(http, FakeCreds(Ok("sk-x")), 8);
        call(&mut c, &request(1, Backend::Api, "slow"));
        call(&mut c, &request(2, Backend::Api, "fast"));
        // The fast reply arrives while the slow one is still held.
        let first = reply(&mut c);
        assert_eq!(first.id, 2);
        release_tx.send(()).unwrap();
        assert_eq!(reply(&mut c).id, 1);
    }

    #[test]
    fn a_malformed_request_with_an_id_gets_bad_request() {
        let mut c = start(FakeHttp::new(), FakeCreds(Ok("sk-x")), 4);
        write_frame(&mut c, &json!({"type": "complete", "id": 9, "backend": "gpt"})).unwrap();
        let r = reply(&mut c);
        assert_eq!(r.id, 9);
        assert_eq!(r.result.unwrap_err().kind, "bad_request");
    }
}
