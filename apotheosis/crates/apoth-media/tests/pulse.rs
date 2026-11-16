// SPDX-License-Identifier: AGPL-3.0-only
//! `PulseSink` against a mock PulseAudio-protocol server on a socket in a temp dir: no
//! real sound server is touched. The mock answers the handshake, counts PCM, records cork,
//! volume and delete, and reports a played position that runs at the wall clock.

use std::io::{BufReader, Cursor, Read};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apoth_media::sink::{AudioSink, PulseSink, Unavailable};
use pulseaudio::protocol::{self, ChannelVolume, Command};

#[derive(Default, Debug)]
struct Seen {
    rate: u32,
    channels: u8,
    first_volume: Option<ChannelVolume>,
    volumes: Vec<ChannelVolume>,
    corks: Vec<bool>,
    pcm: u64,
    flushes: u32,
    deleted: bool,
    role_video: bool,
}

/// Write errors are ignored: the client deletes its stream and closes without waiting.
fn serve(l: UnixListener, seen: Arc<Mutex<Seen>>) {
    let Ok((s, _)) = l.accept() else { return };
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    let v = protocol::MAX_VERSION;
    let mut started: Option<Instant> = None;
    let mut played_before = 0i64;
    let mut byte_rate = 1i64;
    loop {
        let Ok(desc) = protocol::read_descriptor(&mut r) else {
            return;
        };
        let mut payload = vec![0; desc.length as usize];
        if r.read_exact(&mut payload).is_err() {
            return;
        }
        if desc.channel != u32::MAX {
            seen.lock().unwrap().pcm += payload.len() as u64;
            continue;
        }
        let Ok((seq, cmd)) = Command::read_tag_prefixed(&mut Cursor::new(&payload[..]), v) else {
            continue;
        };
        let mut st = seen.lock().unwrap();
        match cmd {
            Command::Auth(_) => {
                let a = protocol::AuthReply {
                    version: v,
                    ..Default::default()
                };
                let _ = protocol::write_reply_message(&mut w, seq, &a, v);
            }
            Command::SetClientName(_) => {
                let a = protocol::SetClientNameReply { client_id: 1 };
                let _ = protocol::write_reply_message(&mut w, seq, &a, v);
            }
            Command::CreatePlaybackStream(p) => {
                st.rate = p.sample_spec.sample_rate;
                st.channels = p.sample_spec.channels;
                st.first_volume = p.cvolume;
                st.role_video = p.props.get(protocol::Prop::MediaRole) == Some(b"video\0");
                byte_rate = i64::from(st.rate) * i64::from(st.channels) * 2;
                let a = protocol::CreatePlaybackStreamReply {
                    channel: 0,
                    stream_index: 7,
                    requested_bytes: 1 << 20,
                    sample_spec: p.sample_spec,
                    channel_map: p.channel_map,
                    ..Default::default()
                };
                let _ = protocol::write_reply_message(&mut w, seq, &a, v);
            }
            Command::CorkPlaybackStream(c) => {
                st.corks.push(c.cork);
                if c.cork {
                    if let Some(s) = started.take() {
                        played_before += s.elapsed().as_micros() as i64 * byte_rate / 1_000_000;
                    }
                } else if started.is_none() {
                    started = Some(Instant::now());
                }
                let _ = protocol::write_ack_message(&mut w, seq);
            }
            Command::SetSinkInputVolume(p) => {
                st.volumes.push(p.volume);
                let _ = protocol::write_ack_message(&mut w, seq);
            }
            Command::FlushPlaybackStream(_) => {
                st.flushes += 1;
                let _ = protocol::write_ack_message(&mut w, seq);
            }
            Command::DeletePlaybackStream(_) => {
                st.deleted = true;
                let _ = protocol::write_ack_message(&mut w, seq);
                return;
            }
            Command::GetPlaybackLatency(p) => {
                let run = started.map_or(0, |s| {
                    s.elapsed().as_micros() as i64 * byte_rate / 1_000_000
                });
                let read = (played_before + run).min(st.pcm as i64);
                let a = protocol::PlaybackLatency {
                    sink_usec: 0,
                    source_usec: 0,
                    playing: started.is_some(),
                    local_time: p.now,
                    remote_time: p.now,
                    write_offset: st.pcm as i64,
                    read_offset: read,
                    underrun_for: 0,
                    playing_for: 0,
                };
                let _ = protocol::write_reply_message(&mut w, seq, &a, v);
            }
            _ => {
                let _ = protocol::write_ack_message(&mut w, seq);
            }
        }
    }
}

fn mock() -> (
    std::path::PathBuf,
    Arc<Mutex<Seen>>,
    std::thread::JoinHandle<()>,
) {
    let dir = std::env::temp_dir().join(format!(
        "apoth-pulse-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native");
    let l = UnixListener::bind(&path).unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = Arc::clone(&seen);
    let t = std::thread::spawn(move || serve(l, s2));
    (path, seen, t)
}

#[test]
fn a_stream_opens_plays_and_reports_its_clock() {
    let (path, seen, t) = mock();
    let mut sink = PulseSink::open(&path, 48_000, 2, 0.0, true).unwrap();
    {
        let s = seen.lock().unwrap();
        assert_eq!((s.rate, s.channels), (48_000, 2));
        assert_eq!(s.first_volume, Some(ChannelVolume::muted(2)));
        assert!(s.role_video);
    }
    assert_eq!(sink.played_us(), None);
    // Half a second of silence, then play.
    sink.write(&vec![0u8; 96_000]);
    sink.cork(false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.played_us().unwrap_or(0) < 100_000 {
        assert!(Instant::now() < deadline, "the clock never advanced");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(sink.queued() <= 96_000);
    sink.set_volume(0.5, false);
    sink.flush();
    std::thread::sleep(Duration::from_millis(100));
    drop(sink);
    t.join().unwrap();
    let s = seen.lock().unwrap();
    assert_eq!(s.pcm, 96_000);
    assert_eq!(s.corks.first(), Some(&false));
    assert_eq!(s.volumes.len(), 1);
    assert_ne!(s.volumes[0], ChannelVolume::muted(2));
    assert_eq!(s.flushes, 1);
    assert!(s.deleted);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn no_server_is_unavailable() {
    let p = std::env::temp_dir().join(format!("apoth-pulse-none-{}", std::process::id()));
    assert!(matches!(
        PulseSink::open(&p, 48_000, 2, 1.0, false),
        Err(Unavailable)
    ));
    assert!(matches!(
        PulseSink::open(&p, 0, 2, 1.0, false),
        Err(Unavailable)
    ));
}
