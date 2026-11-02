// SPDX-License-Identifier: AGPL-3.0-only

//! Capture-grant client (ADR 0085): asks abyss, over
//! `$XDG_RUNTIME_DIR/eclipse/portal.sock`, to show its compositor-drawn
//! confirm and mint a grant for one stream.
//!
//! Wire: one request per connection; each message is a `u32` big-endian
//! length followed by that many bytes of postcard (`CaptureConfirm` out,
//! `CaptureAnswer` back). The abyss side is TCB and not implemented here.
//!
//! Fail-closed: everything except an explicit `Granted` is an `Err`
//! (connect failure, bad peer, oversize/garbled/short frames, timeout, EOF).
//! The answer is a claim until abyss enforces it; the daemon never treats a
//! grant id as authority by itself.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ecl_portal_proto::{CaptureAnswer, CaptureConfirm};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// The user has to act on the modal; abyss has its own (shorter) timeout and
/// answers `Timeout`, this is the backstop.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Frames are tiny; anything bigger is a protocol violation.
const MAX_FRAME: u32 = 16 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GrantError {
    #[error("capture denied")]
    Denied,
    #[error("confirm timed out")]
    Timeout,
    #[error("portal.sock unavailable: {0}")]
    Unavailable(String),
    #[error("protocol error: {0}")]
    Protocol(&'static str),
}

pub fn socket_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("eclipse/portal.sock"))
}

/// The socket must be a socket owned by us (the runtime dir is 0700, this is
/// defence in depth against a planted path).
fn check_socket(path: &Path) -> Result<(), GrantError> {
    let md = std::fs::symlink_metadata(path).map_err(|e| GrantError::Unavailable(e.to_string()))?;
    if !md.file_type().is_socket() {
        return Err(GrantError::Unavailable("not a socket".into()));
    }
    if md.uid() != rustix::process::geteuid().as_raw() {
        return Err(GrantError::Unavailable(
            "socket owned by another uid".into(),
        ));
    }
    Ok(())
}

pub fn encode(req: &CaptureConfirm) -> Result<Vec<u8>, GrantError> {
    let body = postcard::to_stdvec(req).map_err(|_| GrantError::Protocol("encode"))?;
    let len = u32::try_from(body.len()).map_err(|_| GrantError::Protocol("oversize"))?;
    if len > MAX_FRAME {
        return Err(GrantError::Protocol("oversize"));
    }
    let mut out = len.to_be_bytes().to_vec();
    out.extend(body);
    Ok(out)
}

async fn exchange(path: &Path, req: &CaptureConfirm) -> Result<CaptureAnswer, GrantError> {
    check_socket(path)?;
    let msg = encode(req)?;
    let mut s = UnixStream::connect(path)
        .await
        .map_err(|e| GrantError::Unavailable(e.to_string()))?;
    s.write_all(&msg)
        .await
        .map_err(|e| GrantError::Unavailable(e.to_string()))?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len)
        .await
        .map_err(|_| GrantError::Protocol("short read"))?;
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_FRAME {
        return Err(GrantError::Protocol("bad length"));
    }
    let mut body = vec![0u8; len as usize];
    s.read_exact(&mut body)
        .await
        .map_err(|_| GrantError::Protocol("short read"))?;
    postcard::from_bytes(&body).map_err(|_| GrantError::Protocol("decode"))
}

/// Ask abyss for a grant. `Ok(grant_id)` only on an explicit `Granted`.
pub async fn request_grant(
    path: &Path,
    req: &CaptureConfirm,
    timeout: Duration,
) -> Result<u64, GrantError> {
    match tokio::time::timeout(timeout, exchange(path, req)).await {
        Err(_) => Err(GrantError::Timeout),
        Ok(Err(e)) => Err(e),
        Ok(Ok(CaptureAnswer::Granted { grant })) => Ok(grant),
        Ok(Ok(CaptureAnswer::Denied)) => Err(GrantError::Denied),
        Ok(Ok(CaptureAnswer::Timeout)) => Err(GrantError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecl_portal_proto::Source;
    use tokio::net::UnixListener;

    fn req() -> CaptureConfirm {
        CaptureConfirm {
            app_id_claim: "org.example.App".into(),
            stream: 5,
            target: Source::Output {
                name: "DP-1".into(),
                width: 1920,
                height: 1080,
            },
            audio: false,
            persistent: false,
        }
    }

    /// Mock abyss: reads one request, answers with raw `reply` bytes.
    fn serve(
        path: &Path,
        reply: Option<Vec<u8>>,
        delay: Duration,
    ) -> tokio::task::JoinHandle<CaptureConfirm> {
        let l = UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            let mut len = [0u8; 4];
            c.read_exact(&mut len).await.unwrap();
            let mut b = vec![0u8; u32::from_be_bytes(len) as usize];
            c.read_exact(&mut b).await.unwrap();
            let got: CaptureConfirm = postcard::from_bytes(&b).unwrap();
            tokio::time::sleep(delay).await;
            if let Some(r) = reply {
                let _ = c.write_all(&r).await;
            }
            got
        })
    }

    fn frame(a: &CaptureAnswer) -> Vec<u8> {
        let b = postcard::to_stdvec(a).unwrap();
        let mut o = (b.len() as u32).to_be_bytes().to_vec();
        o.extend(b);
        o
    }

    #[tokio::test]
    async fn granted_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("portal.sock");
        let h = serve(
            &p,
            Some(frame(&CaptureAnswer::Granted { grant: 77 })),
            Duration::ZERO,
        );
        assert_eq!(
            request_grant(&p, &req(), Duration::from_secs(2)).await,
            Ok(77)
        );
        assert_eq!(h.await.unwrap(), req());
    }

    #[tokio::test]
    async fn denied_and_timeout_answers() {
        for (a, want) in [
            (CaptureAnswer::Denied, GrantError::Denied),
            (CaptureAnswer::Timeout, GrantError::Timeout),
        ] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("portal.sock");
            let _h = serve(&p, Some(frame(&a)), Duration::ZERO);
            assert_eq!(
                request_grant(&p, &req(), Duration::from_secs(2)).await,
                Err(want)
            );
        }
    }

    #[tokio::test]
    async fn fail_closed_on_garbage_eof_oversize_slow_and_missing() {
        // garbage body
        let mut garbage = 3u32.to_be_bytes().to_vec();
        garbage.extend([0xff, 0xff, 0xff]);
        // oversize length
        let big = (MAX_FRAME + 1).to_be_bytes().to_vec();
        // zero length
        let zero = 0u32.to_be_bytes().to_vec();
        for reply in [Some(garbage), Some(big), Some(zero), None] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("portal.sock");
            let _h = serve(&p, reply, Duration::ZERO);
            let r = request_grant(&p, &req(), Duration::from_secs(2)).await;
            assert!(matches!(r, Err(GrantError::Protocol(_))), "{r:?}");
        }
        // slow peer
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("portal.sock");
        let _h = serve(
            &p,
            Some(frame(&CaptureAnswer::Granted { grant: 1 })),
            Duration::from_secs(5),
        );
        assert_eq!(
            request_grant(&p, &req(), Duration::from_millis(50)).await,
            Err(GrantError::Timeout)
        );
        // no socket
        let r = request_grant(&d.path().join("nope"), &req(), Duration::from_secs(1)).await;
        assert!(matches!(r, Err(GrantError::Unavailable(_))));
    }

    #[tokio::test]
    async fn refuses_non_socket_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("portal.sock");
        std::fs::write(&p, b"x").unwrap();
        let r = request_grant(&p, &req(), Duration::from_secs(1)).await;
        assert!(matches!(r, Err(GrantError::Unavailable(_))));
    }
}
