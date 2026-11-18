//! `genie-process`: child processes bound to the daemon (spec/01-architecture.md §1.1).
//!
//! **All children are bound to the daemon.** Each spawned tree gets its own containment:
//!
//! * **Windows:** a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The daemon owns the
//!   only handle, so when the daemon exits (cleanly or by crash) the OS kills the tree.
//! * **macOS/Linux:** the child leads its own process group; [`ProcessTree::kill_tree`] and
//!   `Drop` send `SIGKILL` to the whole group. On Linux the daemon also becomes a child
//!   subreaper ([`become_subreaper`]) so orphaned grandchildren are reaped.
//!
//! One tree per spawn means killing a shell command never touches the browser or another
//! command (Chrome gets its own tree too).
//!
//! [`pipe`] is pipe mode (06 §13.4): piped stdio, exact/scrubbed environment, output head +
//! tail + spill file, deadlines, kill-tree on cancel and a [`pipe::SpawnHook`] for the
//! sandbox. [`pty`] (feature `pty`) is PTY mode for the Terminal tab (D55): replay ring,
//! VT checkpoint, bounded writer, kill-tree.
//! Code adapted from `opencode-pty` will carry its MIT notice (C3); pipe mode here is
//! written against its documented semantics (KB ch.16), not copied.

pub mod pipe;
pub mod ports;
#[cfg(feature = "pty")]
pub mod pty;

use std::io;
use std::process::ExitStatus;

use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

/// A spawned child and everything it starts.
pub struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: windows::Job,
    #[cfg(unix)]
    pgid: i32,
    killed: bool,
}

/// How a cancellable wait ended.
#[derive(Debug)]
pub enum WaitOutcome {
    Exited(ExitStatus),
    /// The token was cancelled; the tree was killed (01 §6).
    Cancelled,
}

/// Spawn `cmd` in a new kill-tree. The child is never left behind if the caller drops
/// the [`ProcessTree`] or the daemon exits.
pub fn spawn_tree(cmd: &mut Command) -> io::Result<ProcessTree> {
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let pgid = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .ok_or_else(|| io::Error::other("child exited before its pid was read"))?;
        Ok(ProcessTree {
            child,
            pgid,
            killed: false,
        })
    }
    #[cfg(windows)]
    {
        // CREATE_NEW_PROCESS_GROUP so console Ctrl events aimed at the daemon don't hit it.
        cmd.creation_flags(windows::CREATE_NEW_PROCESS_GROUP | windows::CREATE_NO_WINDOW);
        let job = windows::Job::new_kill_on_close()?;
        let child = cmd.spawn()?;
        // Known gap: anything the child starts before this call escapes the job. Spawning
        // suspended needs the primary thread handle, which std/tokio don't expose; the
        // PTY/pipe runner port (D45) closes this with a raw CreateProcessW.
        if let Some(h) = child.raw_handle() {
            job.assign(h)?;
        }
        Ok(ProcessTree {
            child,
            job,
            killed: false,
        })
    }
}

impl ProcessTree {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// A handle for [`ports::TreeProbe::listening_ports`]. On Windows it snapshots the
    /// job's process ids, so take a fresh one for each check.
    pub fn probe(&self) -> ports::TreeProbe {
        ports::TreeProbe {
            #[cfg(unix)]
            pgid: self.pgid,
            #[cfg(windows)]
            pids: self.job.process_ids(),
        }
    }

    /// Kill the child and all its descendants. Idempotent.
    pub fn kill_tree(&mut self) -> io::Result<()> {
        if self.killed {
            return Ok(());
        }
        self.killed = true;
        #[cfg(unix)]
        {
            // SAFETY: plain syscall; negative pid targets the process group we created.
            let rc = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(err);
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.job.terminate()
        }
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Wait for exit, or kill the tree when `token` is cancelled.
    pub async fn wait_or_cancel(&mut self, token: &CancellationToken) -> io::Result<WaitOutcome> {
        tokio::select! {
            status = self.child.wait() => status.map(WaitOutcome::Exited),
            () = token.cancelled() => {
                self.kill_tree()?;
                // Reap the direct child so it doesn't linger as a zombie.
                let _ = self.child.wait().await;
                Ok(WaitOutcome::Cancelled)
            }
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Err(e) = self.kill_tree() {
            tracing::debug!(error = %e, "kill_tree on drop failed");
        }
    }
}

/// Linux: make the daemon a child subreaper so orphaned grandchildren are reparented to
/// it and can be reaped (01 §1.1). No-op elsewhere.
pub fn become_subreaper() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl with integer args has no memory-safety preconditions.
        let rc = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    };

    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub struct Job(HANDLE);

    // SAFETY: a job handle is a kernel object handle, usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new_kill_on_close() -> io::Result<Job> {
            // SAFETY: null attributes and name are allowed.
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(h);
            // SAFETY: zeroed is a valid initial value for this plain C struct.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: pointer and size describe `info`, which outlives the call.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    u32::try_from(std::mem::size_of_val(&info)).unwrap_or(u32::MAX),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub fn assign(&self, process: RawHandle) -> io::Result<()> {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, process as HANDLE) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// The ids of the processes in the job (up to 1024). Empty on error.
        pub fn process_ids(&self) -> Vec<u32> {
            // JOBOBJECT_BASIC_PROCESS_ID_LIST: two u32 counts, then ULONG_PTR ids.
            const MAX: usize = 1024;
            let header = 8usize.div_ceil(std::mem::size_of::<usize>());
            let mut buf = vec![0usize; header + MAX];
            let bytes = u32::try_from(std::mem::size_of_val(buf.as_slice())).unwrap_or(u32::MAX);
            // SAFETY: the buffer is writable for `bytes` bytes and usize-aligned, as the
            // struct requires; the call writes at most that much.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr().cast(),
                    bytes,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return vec![];
            }
            // SAFETY: the first 8 bytes are the two u32 counts written by the call.
            let listed = unsafe { *buf.as_ptr().cast::<u32>().add(1) } as usize;
            buf[header..header + listed.min(MAX)]
                .iter()
                .filter_map(|&p| u32::try_from(p).ok())
                .collect()
        }

        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: valid job handle.
            let ok = unsafe { TerminateJobObject(self.0, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; closing the last handle kills the tree.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn long_running() -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 > NUL"]);
            c
        }
        #[cfg(unix)]
        {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 30"]);
            c
        }
    }

    #[tokio::test]
    async fn cancel_kills_tree() {
        let mut tree = spawn_tree(&mut long_running()).expect("spawn");
        let token = CancellationToken::new();
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            token.cancel();
        };
        let waiter = tokio::time::timeout(Duration::from_secs(10), tree.wait_or_cancel(&token));
        let ((), out) = tokio::join!(canceller, waiter);
        let out = out.expect("did not hang").expect("wait ok");
        assert!(matches!(out, WaitOutcome::Cancelled));
    }

    #[tokio::test]
    async fn exit_is_reported() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 3"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 3"]);
            c
        };
        let mut tree = spawn_tree(&mut cmd).expect("spawn");
        let out = tree
            .wait_or_cancel(&CancellationToken::new())
            .await
            .expect("wait");
        match out {
            WaitOutcome::Exited(s) => assert_eq!(s.code(), Some(3)),
            WaitOutcome::Cancelled => panic!("not cancelled"),
        }
    }
}
