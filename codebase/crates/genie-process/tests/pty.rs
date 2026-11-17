//! PTY mode against a real `sh` (Unix): write, read, resize, reattach from an offset,
//! truncation → checkpoint, close kills the tree, exit code.
#![cfg(all(unix, feature = "pty"))]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use genie_process::pipe::Env;
use genie_process::pty::{Pty, PtyEvent, PtySpec};

fn sh() -> PtySpec {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env.insert("PS1".to_string(), "$ ".to_string());
    env.insert("HOME".to_string(), std::env::temp_dir().display().to_string());
    PtySpec::new("/bin/sh").env(Env::exact(env)).size(80, 24)
}

/// Poll the replay until `pred` holds (or panic after 10 s).
fn wait_for(pty: &Pty, what: &str, pred: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, _, bytes) = pty.read_from(0);
        let text = String::from_utf8_lossy(&bytes).to_string();
        if pred(&text) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {text:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn write_read_and_live_events() {
    let pty = Pty::spawn(sh()).expect("spawn");
    let mut att = pty.attach(0);
    assert!(!att.truncated);
    pty.write(b"echo hel''lo-pty\n").expect("write");
    // Live events carry contiguous offsets.
    let mut seen = String::from_utf8_lossy(&att.replay).to_string();
    let mut next = att.end_offset;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains("hello-pty") {
        let ev = tokio::time::timeout_at(deadline, att.events.recv())
            .await
            .expect("output in time")
            .expect("event");
        if let PtyEvent::Output { start, data } = ev {
            assert_eq!(start, next, "gap-free offsets");
            next = start + data.len() as u64;
            seen.push_str(&String::from_utf8_lossy(&data));
        }
    }
    assert!(pty.screen_text().contains("hello-pty"));
}

#[tokio::test]
async fn resize_reaches_the_child() {
    let pty = Pty::spawn(sh()).expect("spawn");
    pty.resize(120, 40).expect("resize");
    pty.write(b"stty size\n").expect("write");
    wait_for(&pty, "stty size", |t| t.contains("40 120"));
}

#[tokio::test]
async fn reattach_from_an_offset_returns_only_newer_bytes() {
    let pty = Pty::spawn(sh()).expect("spawn");
    pty.write(b"echo FIRST\n").expect("write");
    wait_for(&pty, "FIRST", |t| t.matches("FIRST").count() >= 2);
    let mark = pty.end_offset();
    pty.write(b"echo SECOND\n").expect("write");
    wait_for(&pty, "SECOND", |t| t.matches("SECOND").count() >= 2);
    let att = pty.attach(mark);
    assert!(!att.truncated);
    assert!(att.checkpoint.is_none());
    let replay = String::from_utf8_lossy(&att.replay);
    assert!(replay.contains("SECOND"), "{replay:?}");
    assert!(!replay.contains("FIRST"), "{replay:?}");
    assert_eq!(att.end_offset, mark + att.replay.len() as u64);
}

#[tokio::test]
async fn truncated_reattach_gets_a_checkpoint() {
    let pty = Pty::spawn(sh().replay_capacity(64)).expect("spawn");
    pty.write(b"echo 0123456789-0123456789-0123456789-0123456789-0123456789-END\n")
        .expect("write");
    let deadline = Instant::now() + Duration::from_secs(10);
    while pty.end_offset() < 200 || !pty.screen_text().contains("789-END\n") {
        assert!(Instant::now() < deadline, "{:?}", pty.screen_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    let att = pty.attach(0);
    assert!(att.truncated);
    assert!(att.replay.is_empty());
    let cp = String::from_utf8_lossy(att.checkpoint.as_deref().expect("checkpoint"));
    assert!(cp.contains("789-END"), "{cp:?}");
    // Repainting a fresh 80x24 screen from the checkpoint reproduces ours.
    let mut vt = vt100::Parser::new(
        std::num::NonZeroU16::new(24).expect("rows"),
        std::num::NonZeroU16::new(80).expect("cols"),
        0,
    );
    vt.process(cp.as_bytes());
    assert_eq!(vt.screen().contents(), pty.screen_text());
}

#[tokio::test]
async fn exit_code_is_reported() {
    let pty = Pty::spawn(PtySpec::new("/bin/sh").args(["-c", "exit 7"])).expect("spawn");
    let code = tokio::time::timeout(Duration::from_secs(10), pty.wait())
        .await
        .expect("exit in time");
    assert_eq!(code, Some(7));
    assert!(pty.write(b"x").is_err());
}

#[tokio::test]
async fn kill_tree_takes_foreground_and_background_jobs() {
    let pty = Pty::spawn(sh()).expect("spawn");
    pty.write(b"sleep 300 & echo BG=$!\n").expect("write");
    let text = wait_for(&pty, "background pid", |t| {
        t.lines().any(|l| l.trim_start().starts_with("BG=") && l.trim().len() > 3)
    });
    let bg: i32 = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("BG="))
        .and_then(|p| p.trim().parse().ok())
        .expect("pid");
    pty.write(b"sleep 301\n").expect("write");
    std::thread::sleep(Duration::from_millis(300));
    let shell = i32::try_from(pty.pid().expect("pid")).expect("i32");
    assert!(alive(bg) && alive(shell));
    pty.kill_tree();
    let code = tokio::time::timeout(Duration::from_secs(10), pty.wait())
        .await
        .expect("exited in time");
    assert_eq!(code, None, "killed by a signal");
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(bg) {
        // A killed child stays a zombie until its (re)parent reaps it; signal 0 still
        // succeeds for zombies, so check the state too.
        if is_zombie(bg) {
            break;
        }
        assert!(Instant::now() < deadline, "background job {bg} survived");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn is_zombie(pid: i32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.trim().is_empty() || s.trim_start().starts_with('Z')
        })
        .unwrap_or(false)
}
