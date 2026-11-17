//! The `shell` tool against real processes (06 §13.1–13.3). Unix only: Windows shells are
//! covered by the selection and script unit tests.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_protocol::ask::Preview;
use genie_protocol::common::{Mode, SessionKind};
use genie_tools::shell::{
    BackgroundJobs, FixedDir, JobFinished, JobStarted, ShellChoice, ShellConfig, ShellKind,
    ShellRuntime,
};
use genie_tools::{
    MemorySpill, NullSink, PermissionGate, PermissionRequest, SpillStore, Tool, ToolCtx, ToolEnv,
    ToolError, ToolOutput,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Default)]
struct Gate {
    seen: Mutex<Vec<PermissionRequest>>,
    deny: bool,
}

#[async_trait]
impl PermissionGate for Gate {
    async fn assert(&self, req: PermissionRequest) -> Result<(), ToolError> {
        self.seen.lock().unwrap().push(req);
        if self.deny {
            Err(ToolError::Declined {
                message: Some("no".into()),
            })
        } else {
            Ok(())
        }
    }
}

struct Jobs {
    started: Mutex<Vec<JobStarted>>,
    tx: mpsc::UnboundedSender<JobFinished>,
}

impl BackgroundJobs for Jobs {
    fn started(&self, job: &JobStarted) {
        self.started.lock().unwrap().push(job.clone());
    }
    fn finished(&self, job: JobFinished) {
        let _ = self.tx.send(job);
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    work: PathBuf,
    rt: ShellRuntime,
    tool: Arc<dyn Tool>,
    jobs: Arc<Jobs>,
    done: mpsc::UnboundedReceiver<JobFinished>,
    spill: Arc<MemorySpill>,
    gate: Arc<Gate>,
}

fn fixture_with(kind: ShellKind, program: &str, edit: impl FnOnce(&mut ShellConfig)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work").canonicalize().unwrap_or_else(|_| {
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        dir.path().join("work").canonicalize().unwrap()
    });
    let (tx, done) = mpsc::unbounded_channel();
    let jobs = Arc::new(Jobs {
        started: Mutex::default(),
        tx,
    });
    let mut cfg = ShellConfig::new(
        Arc::new(FixedDir(work.clone())),
        TaskTracker::new(),
        CancellationToken::new(),
    )
    .with_jobs(jobs.clone());
    cfg.temp_dir = dir.path().join("tmp");
    cfg.shell = Some(ShellChoice {
        kind,
        program: program.into(),
    });
    edit(&mut cfg);
    let rt = ShellRuntime::new(cfg);
    let tool = rt.tools().remove(0);
    Fixture {
        _dir: dir,
        work,
        rt,
        tool,
        jobs,
        done,
        spill: Arc::new(MemorySpill::new()),
        gate: Arc::new(Gate::default()),
    }
}

fn fixture() -> Fixture {
    fixture_with(ShellKind::Bash, "/bin/bash", |_| {})
}

impl Fixture {
    fn ctx(&self, cancel: CancellationToken) -> ToolCtx {
        let mut env = ToolEnv::new("ses", SessionKind::Code);
        env.services.spill = Some(self.spill.clone());
        ToolCtx {
            env: Arc::new(env),
            turn_id: "trn".into(),
            call_id: "call".into(),
            tool_name: "shell".into(),
            cancel,
            permissions: self.gate.clone(),
            events: Arc::new(NullSink),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        self.tool
            .execute(input, self.ctx(CancellationToken::new()))
            .await
            .map(|o| match o {
                ToolOutput::Text(t) => t,
                ToolOutput::Json(v) => v.to_string(),
            })
    }

    async fn sh(&self, cmd: &str) -> String {
        self.run(json!({ "command": cmd })).await.unwrap()
    }

    async fn finished(&mut self) -> JobFinished {
        tokio::time::timeout(Duration::from_secs(20), self.done.recv())
            .await
            .expect("background job did not finish")
            .unwrap()
    }
}

#[tokio::test]
async fn runs_commands_and_persists_cd_only() {
    let f = fixture();
    assert_eq!(f.sh("echo hi").await, "hi\n");
    std::fs::create_dir(f.work.join("sub")).unwrap();
    let out = f.sh("cd sub && export FOO=1").await;
    assert!(out.contains("[cwd is now"), "{out}");
    let pwd = f.sh("pwd -P").await;
    assert_eq!(Path::new(pwd.trim()), f.work.join("sub"));
    // Only the cwd persists, not variables.
    assert_eq!(f.sh("echo \"x${FOO}x\"").await, "xx\n");
    assert_eq!(f.rt.cwd("ses").unwrap(), f.work.join("sub"));
    // A `cd` inside a subshell doesn't move the session.
    f.sh("(cd /)").await;
    assert_eq!(f.rt.cwd("ses").unwrap(), f.work.join("sub"));

    // Permission request: `Bash(<command>)` with the cwd in the preview.
    let seen = f.gate.seen.lock().unwrap().clone();
    let last = seen.last().unwrap();
    assert_eq!(last.resource.as_deref(), Some("Bash((cd /))"));
    assert_eq!(
        last.preview,
        Some(Preview::Command {
            command: "(cd /)".into(),
            cwd: Some(f.work.join("sub").display().to_string()),
        })
    );
    let a = genie_tools::shell::authz_from_request(last.resource.as_deref(), last.preview.as_ref())
        .unwrap();
    assert_eq!(a.parts, Some(vec!["cd /".to_string()]));
}

#[tokio::test]
async fn exit_codes_and_stderr_are_reported() {
    let f = fixture();
    let out = f.sh("echo out; echo err >&2; exit 3").await;
    assert!(out.contains("out\n") && out.contains("err\n"), "{out}");
    assert!(out.ends_with("[exit code 3]"), "{out}");
    assert_eq!(f.sh("true").await, "(no output)");
}

#[tokio::test]
async fn timeout_moves_the_command_to_the_background() {
    let mut f = fixture();
    let out = f
        .run(json!({"command": "echo started; sleep 1; echo done", "timeout_ms": 300}))
        .await
        .unwrap();
    assert!(out.contains("moved to the background as job bg_"), "{out}");
    assert!(out.contains("don't poll"), "{out}");
    assert!(out.contains("started"), "output so far: {out}");
    let started = f.jobs.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].command, "echo started; sleep 1; echo done");
    assert_eq!(f.rt.running_jobs("ses"), vec![started[0].id.clone()]);
    let done = f.finished().await;
    assert_eq!(done.id, started[0].id);
    assert_eq!((done.exit_code, done.killed), (Some(0), false));
    assert_eq!(done.code_text(), "0");
    assert!(done.output.contains("started\ndone"), "{}", done.output);
    assert!(f.rt.running_jobs("ses").is_empty());
}

#[tokio::test]
async fn run_in_background_and_end_session_kills_jobs() {
    let mut f = fixture();
    let out = f
        .run(json!({"command": "echo bg", "run_in_background": true}))
        .await
        .unwrap();
    assert!(
        out.starts_with("Started in the background as job bg_"),
        "{out}"
    );
    let done = f.finished().await;
    assert_eq!(done.output, "bg\n");

    f.run(json!({"command": "sleep 30", "run_in_background": true}))
        .await
        .unwrap();
    assert_eq!(f.rt.running_jobs("ses").len(), 1);
    f.rt.end_session("ses");
    let done = f.finished().await;
    assert!(done.killed, "{done:?}");
    assert_eq!(done.code_text(), "killed");
}

#[tokio::test]
async fn kill_job_stops_one_job() {
    let mut f = fixture();
    f.run(json!({"command": "sleep 30", "run_in_background": true}))
        .await
        .unwrap();
    let id = f.rt.running_jobs("ses").remove(0);
    assert!(f.rt.kill_job(&id));
    let done = f.finished().await;
    assert!(done.killed && done.id == id);
    assert!(!f.rt.kill_job(&id));
}

#[tokio::test]
async fn kill_session_jobs_keeps_the_cwd() {
    let mut f = fixture();
    std::fs::create_dir_all(f.work.join("sub")).unwrap();
    f.run(json!({"command": "cd sub"})).await.unwrap();
    let cwd = f.rt.cwd("ses");
    assert!(cwd.as_ref().is_some_and(|c| c.ends_with("sub")), "{cwd:?}");
    f.run(json!({"command": "sleep 30", "run_in_background": true}))
        .await
        .unwrap();
    f.run(json!({"command": "sleep 31", "run_in_background": true}))
        .await
        .unwrap();
    assert_eq!(f.rt.kill_session_jobs("other"), 0);
    assert_eq!(f.rt.kill_session_jobs("ses"), 2);
    assert!(f.finished().await.killed);
    assert!(f.finished().await.killed);
    assert_eq!(f.rt.cwd("ses"), cwd);
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn cancel_kills_the_whole_tree() {
    let f = fixture();
    let pidfile = f.work.join("pid");
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c2.cancel();
    };
    let cmd = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
    let (res, ()) = tokio::join!(
        f.tool.execute(json!({ "command": cmd }), f.ctx(cancel)),
        canceller
    );
    assert_eq!(res.unwrap_err(), ToolError::Cancelled);
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut gone = false;
    for _ in 0..40 {
        if !alive(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "grandchild {pid} survived the cancel");
}

#[tokio::test]
async fn long_output_is_cut_to_head_and_tail_and_spilled() {
    let f = fixture();
    let out = f.sh("seq 1 5000").await;
    assert!(out.starts_with("1\n2\n3\n"), "{}", &out[..20]);
    assert!(out.ends_with("4999\n5000"), "{}", &out[out.len() - 20..]);
    let marker = "[… lines 401–4600 of 5000 omitted; full output: spill_1]";
    assert!(out.contains(marker), "{}", &out[..out.len().min(4000)]);
    let full = f.spill.read("ses", "spill_1").unwrap();
    assert_eq!(full.lines().count(), 5000);
}

#[tokio::test]
async fn declined_commands_never_run() {
    let mut f = fixture();
    f.gate = Arc::new(Gate {
        seen: Mutex::default(),
        deny: true,
    });
    let err = f.run(json!({"command": "touch marker"})).await.unwrap_err();
    assert_eq!(
        err,
        ToolError::Declined {
            message: Some("no".into())
        }
    );
    assert!(!f.work.join("marker").exists());
}

#[tokio::test]
async fn spawn_failure_is_safe_to_retry() {
    let f = fixture_with(ShellKind::Bash, "/nonexistent/bash", |_| {});
    let err = f.run(json!({"command": "echo hi"})).await.unwrap_err();
    assert!(
        matches!(
            err,
            ToolError::Failed {
                may_have_run: false,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn timeout_without_background_support_kills() {
    let f = fixture_with(ShellKind::Bash, "/bin/bash", |c| c.jobs = None);
    let err = f
        .run(json!({"command": "sleep 5", "timeout_ms": 200}))
        .await
        .unwrap_err();
    assert_eq!(err, ToolError::Timeout { after_s: 0 });
    let err = f
        .run(json!({"command": "echo x", "run_in_background": true}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed { .. }));
}

#[tokio::test]
async fn zsh_persists_cd_and_keeps_unmatched_globs() {
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    let f = fixture_with(ShellKind::Zsh, "/bin/zsh", |_| {});
    assert_eq!(f.sh("echo nomatch*zzz").await, "nomatch*zzz\n");
    std::fs::create_dir(f.work.join("z")).unwrap();
    f.sh("cd z").await;
    assert_eq!(Path::new(f.sh("pwd -P").await.trim()), f.work.join("z"));
    // `exit` still records the cwd (EXIT trap).
    f.sh("cd .. ; exit 4").await;
    assert_eq!(f.rt.cwd("ses").unwrap(), f.work);
}

#[tokio::test]
async fn availability_and_input_validation() {
    let f = fixture();
    let mut env = ToolEnv::new("s", SessionKind::Chat);
    assert!(!f.tool.availability(&env).is_available());
    env.kind = SessionKind::Code;
    assert!(f.tool.availability(&env).is_available());
    env.mode = Mode::Plan;
    assert!(!f.tool.availability(&env).is_available());
    assert!(f.tool.description(&env).contains("bash"));
    let err = f.run(json!({"command": "  "})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput { .. }));
    let err = f.run(json!({"cmd": "ls"})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput { .. }));
}
