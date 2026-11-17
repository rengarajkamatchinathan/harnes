//! Pipe mode (06 §13.4) against real child processes.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use genie_process::pipe::{
    Env, EnvBase, OutputCaps, OutputCollector, PipeSpec, RunOutcome, SpawnHook, StdinMode, run,
    spawn_pipe,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
fn sh(script: &str) -> PipeSpec {
    PipeSpec::new("sh").args(["-c", script])
}

#[cfg(windows)]
fn sh(script: &str) -> PipeSpec {
    PipeSpec::new("cmd").args(["/C", script])
}

#[test]
fn env_resolution_scrubs_and_overrides() {
    let parent = vec![
        (OsString::from("PATH"), OsString::from("/bin")),
        (OsString::from("GENIE_TOKEN_FILE"), OsString::from("x")),
        (OsString::from("SECRET"), OsString::from("s")),
        (OsString::from("HOME"), OsString::from("/home/u")),
    ];
    let inherit = Env::inherit().with("FOO", "1").resolve(parent.clone());
    assert!(inherit.contains_key(&OsString::from("PATH")));
    assert!(inherit.contains_key(&OsString::from("SECRET")));
    assert!(!inherit.contains_key(&OsString::from("GENIE_TOKEN_FILE")));
    assert_eq!(inherit[&OsString::from("FOO")], OsString::from("1"));

    let mut minimal = Env::minimal();
    minimal.remove.push("HOME".into());
    let m = minimal.resolve(parent.clone());
    assert!(m.contains_key(&OsString::from("PATH")));
    assert!(!m.contains_key(&OsString::from("SECRET")));
    assert!(!m.contains_key(&OsString::from("HOME")));

    let exact = Env::exact(BTreeMap::from([("A".to_string(), "b".to_string())])).resolve(parent);
    assert_eq!(exact.len(), 1);

    let allow = Env {
        base: EnvBase::Allowlist(vec!["SECRET".into()]),
        remove: vec![],
        set: BTreeMap::from([("SECRET".to_string(), "override".to_string())]),
    }
    .resolve(vec![(OsString::from("SECRET"), OsString::from("s"))]);
    assert_eq!(allow[&OsString::from("SECRET")], OsString::from("override"));
}

#[cfg(unix)]
#[tokio::test]
async fn child_gets_exactly_the_given_env() {
    let spec = PipeSpec::new("/usr/bin/env").env(Env::exact(BTreeMap::from([(
        "ONLY".to_string(),
        "me".to_string(),
    )])));
    let r = run(
        spec,
        None,
        OutputCaps::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("run");
    assert_eq!(r.output.text().trim(), "ONLY=me");
}

#[tokio::test]
async fn combined_output_head_tail_and_spill_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let file = dir.path().join("out.log");
    #[cfg(unix)]
    let script =
        "i=0; while [ $i -lt 200 ]; do echo line$i; i=$((i+1)); done; echo err 1>&2; exit 4";
    #[cfg(windows)]
    let script = "for /L %i in (0,1,199) do @echo line%i & echo err 1>&2 & exit /b 4";
    let caps = OutputCaps {
        head_bytes: 20,
        tail_bytes: 20,
        file: Some(file.clone()),
        file_cap: 100,
    };
    let r = run(sh(script), None, caps, None, &CancellationToken::new())
        .await
        .expect("run");
    match r.outcome {
        RunOutcome::Exited(s) => assert_eq!(s.code(), Some(4)),
        o => panic!("unexpected {o:?}"),
    }
    let o = &r.output;
    assert!(o.head.starts_with("line0"));
    assert_eq!(o.head.len(), 20);
    assert_eq!(o.tail.len(), 20);
    assert!(o.omitted_bytes > 0);
    assert!(o.stderr_bytes >= 3);
    assert!(o.lines >= 201);
    assert!(o.text().contains("bytes omitted"));
    assert!(o.file_truncated);
    let spilled = std::fs::read(&file).expect("spill");
    assert_eq!(spilled.len(), 100);
}

#[tokio::test]
async fn timeout_leaves_child_running_then_resumes() {
    #[cfg(unix)]
    let script = "echo start; sleep 1; echo done";
    #[cfg(windows)]
    let script = "echo start & ping -n 2 127.0.0.1 > NUL & echo done";
    let mut child = spawn_pipe(sh(script), None).expect("spawn");
    let mut out = OutputCollector::new(OutputCaps::default())
        .await
        .expect("c");
    let cancel = CancellationToken::new();
    let o = child
        .collect(
            &mut out,
            Some(Instant::now() + Duration::from_millis(200)),
            &cancel,
        )
        .await
        .expect("collect");
    assert_eq!(o, RunOutcome::TimedOut);
    assert!(out.summary().text().contains("start"));
    // Backgrounded: keep collecting until it finishes.
    let o = child
        .collect(&mut out, None, &cancel)
        .await
        .expect("collect");
    assert!(matches!(o, RunOutcome::Exited(s) if s.success()));
    assert!(out.summary().text().contains("done"));
}

#[tokio::test]
async fn run_timeout_kills_tree() {
    #[cfg(unix)]
    let script = "sleep 30";
    #[cfg(windows)]
    let script = "ping -n 30 127.0.0.1 > NUL";
    let started = std::time::Instant::now();
    let r = run(
        sh(script),
        None,
        OutputCaps::default(),
        Some(Duration::from_millis(200)),
        &CancellationToken::new(),
    )
    .await
    .expect("run");
    assert_eq!(r.outcome, RunOutcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_grandchildren_holding_the_pipe() {
    // The grandchild keeps stdout open; cancel must not hang on it.
    let mut child = spawn_pipe(sh("(sleep 30; echo late) & echo up; wait"), None).expect("spawn");
    let mut out = OutputCollector::new(OutputCaps::default())
        .await
        .expect("c");
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.cancel();
    };
    let (o, ()) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(10),
            child.collect(&mut out, None, &cancel)
        ),
        canceller
    );
    assert_eq!(o.expect("no hang").expect("collect"), RunOutcome::Cancelled);
    assert!(out.summary().text().contains("up"));
}

#[cfg(unix)]
#[tokio::test]
async fn exited_child_with_lingering_descendant_returns() {
    // The direct child exits while a background descendant holds stdout open.
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            sh("sleep 30 & echo bye"),
            None,
            OutputCaps::default(),
            None,
            &CancellationToken::new(),
        ),
    )
    .await
    .expect("no hang")
    .expect("run");
    assert!(matches!(r.outcome, RunOutcome::Exited(s) if s.success()));
    assert!(r.output.text().contains("bye"));
}

#[tokio::test]
async fn piped_stdin_round_trip() {
    #[cfg(unix)]
    let spec = PipeSpec::new("cat").stdin(StdinMode::Piped);
    #[cfg(windows)]
    let spec = PipeSpec::new("findstr")
        .args(["x*"])
        .stdin(StdinMode::Piped);
    let mut child = spawn_pipe(spec, None).expect("spawn");
    child
        .write_stdin_and_close(b"hello\n")
        .await
        .expect("write");
    let mut out = OutputCollector::new(OutputCaps::default())
        .await
        .expect("c");
    let o = child
        .collect(&mut out, None, &CancellationToken::new())
        .await
        .expect("collect");
    assert!(matches!(o, RunOutcome::Exited(_)));
    assert!(out.summary().text().contains("hello"));
}

#[tokio::test]
async fn spawn_failure_is_safe_to_retry() {
    let err = run(
        PipeSpec::new("genie-definitely-not-a-program"),
        None,
        OutputCaps::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect_err("must fail");
    assert!(err.safe_to_retry());
    assert!(err.to_string().contains("safe to retry"));
}

struct EchoWrap;

impl SpawnHook for EchoWrap {
    fn wrap(&self, spec: &mut PipeSpec) {
        // A sandbox launcher would go here; this one prints what it would have run.
        let mut args = vec![OsString::from("wrapped:"), spec.program.clone()];
        args.append(&mut spec.args);
        #[cfg(unix)]
        {
            spec.program = "echo".into();
            spec.args = args;
        }
        #[cfg(windows)]
        {
            spec.program = "cmd".into();
            let mut a = vec![OsString::from("/C"), OsString::from("echo")];
            a.extend(args);
            spec.args = a;
        }
    }
}

#[tokio::test]
async fn spawn_hook_wraps_program() {
    let r = run(
        PipeSpec::new("rm").args(["-rf", "/nonexistent"]),
        Some(&EchoWrap),
        OutputCaps::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("run");
    assert!(r.output.text().contains("wrapped: rm -rf /nonexistent"));
}
