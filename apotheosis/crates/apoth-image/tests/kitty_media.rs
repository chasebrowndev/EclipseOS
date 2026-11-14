// SPDX-License-Identifier: AGPL-3.0-only
//! T-18 §12: a nonexistent path, a FIFO, a /proc path and a symlink all give the identical
//! failure and never block; denial gives no effect.
use apoth_image::Limits;
use apoth_image::kitty::*;
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("apoth-image-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn load(m: Medium, p: &[u8], s: u32, o: u32, pol: &MediaPolicy) -> MediaResult {
    load_media(m, p, s, o, pol, &Limits::default())
}

#[test]
fn regular_file_with_size_and_offset() {
    let d = tmpdir("reg");
    let f = d.join("a.rgb");
    std::fs::write(&f, b"0123456789").unwrap();
    let pol = MediaPolicy::local_allow(1 << 20);
    let p = f.to_str().unwrap().as_bytes();
    assert_eq!(
        load(Medium::File, p, 0, 0, &pol),
        MediaResult::Data(b"0123456789".to_vec())
    );
    assert_eq!(
        load(Medium::File, p, 3, 2, &pol),
        MediaResult::Data(b"234".to_vec())
    );
    assert_eq!(
        load(Medium::File, p, 100, 8, &pol),
        MediaResult::Data(b"89".to_vec())
    );
    assert_eq!(load(Medium::File, p, 0, 11, &pol), MediaResult::Failed);
    assert!(f.exists(), "f= files are never deleted");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn identical_failure_for_every_cause() {
    let d = tmpdir("fail");
    let pol = MediaPolicy::local_allow(1 << 20);
    std::fs::write(d.join("real"), b"x").unwrap();
    std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
    std::os::unix::fs::symlink("/proc/self/status", d.join("plink")).unwrap();
    let fifo = d.join("fifo");
    let have_fifo = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    let mut cases: Vec<Vec<u8>> = vec![
        d.join("nonexistent").to_str().unwrap().into(),
        d.join("link").to_str().unwrap().into(),
        d.join("plink").to_str().unwrap().into(),
        d.to_str().unwrap().into(), // a directory
        b"/proc/self/status".to_vec(),
        b"/proc/self/mem".to_vec(),
        b"/sys/kernel/notes".to_vec(),
        b"/dev/zero".to_vec(),
        b"/dev/stdin".to_vec(),
        b"relative/path".to_vec(),
        format!("{}/../real", d.display()).into_bytes(),
        format!("{}/sub/../real", d.display()).into_bytes(),
        b"/tmp/\0/x".to_vec(),
        Vec::new(),
        vec![b'a'; 5000],
    ];
    if have_fifo {
        cases.push(fifo.to_str().unwrap().into());
    }
    for c in &cases {
        assert_eq!(
            load(Medium::File, c, 0, 0, &pol),
            MediaResult::Failed,
            "{:?}",
            String::from_utf8_lossy(c)
        );
    }
    // symlinked directory pointing into /proc
    std::os::unix::fs::symlink("/proc/self", d.join("pdir")).unwrap();
    let via = format!("{}/pdir/status", d.display());
    assert_eq!(
        load(Medium::File, via.as_bytes(), 0, 0, &pol),
        MediaResult::Failed
    );
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn quota_and_cap_checked_before_read() {
    let d = tmpdir("quota");
    let f = d.join("big");
    std::fs::write(&f, vec![0u8; 4096]).unwrap();
    let p = f.to_str().unwrap().as_bytes();
    assert_eq!(
        load(Medium::File, p, 0, 0, &MediaPolicy::local_allow(100)),
        MediaResult::Failed
    );
    assert_eq!(
        load(Medium::File, p, 50, 0, &MediaPolicy::local_allow(100)),
        MediaResult::Data(vec![0; 50])
    );
    let small = Limits {
        max_decoded_bytes: 10,
        ..Limits::default()
    };
    let r = load_media(
        Medium::File,
        p,
        0,
        0,
        &MediaPolicy::local_allow(1 << 20),
        &small,
    );
    assert_eq!(r, MediaResult::Failed);
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn denied_has_no_effect() {
    let d = tmpdir("deny");
    let f = d.join("tty-graphics-protocol-x");
    std::fs::write(&f, b"data").unwrap();
    let mut pol = MediaPolicy::local_allow(1 << 20);
    pol.temp_dirs = vec![d.clone()];
    pol.decision = Decision::Deny;
    let p = f.to_str().unwrap().as_bytes();
    assert_eq!(load(Medium::TempFile, p, 0, 0, &pol), MediaResult::Denied);
    assert!(
        f.exists(),
        "denied transmission must not delete the temp file"
    );
    pol.decision = Decision::Allow;
    assert_eq!(
        load(Medium::TempFile, p, 0, 0, &pol),
        MediaResult::Data(b"data".to_vec())
    );
    assert!(!f.exists(), "temp file consumed after a successful read");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn temp_file_rules() {
    let d = tmpdir("temp");
    let ok = d.join("x-tty-graphics-protocol-y");
    let wrong_name = d.join("plain");
    std::fs::write(&ok, b"1").unwrap();
    std::fs::write(&wrong_name, b"1").unwrap();
    let mut pol = MediaPolicy::local_allow(1 << 20);
    // not under a known temp dir
    pol.temp_dirs = vec!["/nonexistent-temp-root".into()];
    assert_eq!(
        load(
            Medium::TempFile,
            ok.to_str().unwrap().as_bytes(),
            0,
            0,
            &pol
        ),
        MediaResult::Failed
    );
    pol.temp_dirs = vec![d.clone()];
    assert_eq!(
        load(
            Medium::TempFile,
            wrong_name.to_str().unwrap().as_bytes(),
            0,
            0,
            &pol
        ),
        MediaResult::Failed
    );
    assert!(wrong_name.exists());
    assert!(matches!(
        load(
            Medium::TempFile,
            ok.to_str().unwrap().as_bytes(),
            0,
            0,
            &pol
        ),
        MediaResult::Data(_)
    ));
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn shm_names() {
    let pol = MediaPolicy::local_allow(1 << 20);
    for bad in [&b""[..], b"/", b"..", b"a/b", b"/a/b", b"\0"] {
        assert_eq!(load(Medium::Shm, bad, 0, 0, &pol), MediaResult::Failed);
    }
    // real /dev/shm object, consumed by the read
    let name = format!("apoth-image-test-{}", std::process::id());
    let path = PathBuf::from("/dev/shm").join(&name);
    if std::fs::write(&path, b"shmdata").is_ok() {
        assert_eq!(
            load(Medium::Shm, name.as_bytes(), 0, 0, &pol),
            MediaResult::Data(b"shmdata".to_vec())
        );
        assert!(!path.exists());
    }
}

#[test]
fn temp_file_under_dev_shm_fails() {
    let name = format!("x-tty-graphics-protocol-{}", std::process::id());
    let path = PathBuf::from("/dev/shm").join(&name);
    if std::fs::write(&path, b"1").is_err() {
        return;
    }
    let mut pol = MediaPolicy::local_allow(1 << 20);
    pol.temp_dirs.push("/dev/shm".into());
    assert_eq!(
        load(
            Medium::TempFile,
            path.to_str().unwrap().as_bytes(),
            0,
            0,
            &pol
        ),
        MediaResult::Failed
    );
    assert!(path.exists());
    let _ = std::fs::remove_file(path);
}
