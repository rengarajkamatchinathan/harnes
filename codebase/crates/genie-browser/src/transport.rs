//! `--remote-debugging-pipe` (06 §10): no TCP debug port, so no other local process can
//! attach. Chrome reads NUL-terminated JSON messages from fd 3 and writes them to fd 4
//! (POSIX); on Windows the two inheritable pipe handles are passed with
//! `--remote-debugging-io-pipes=<read>,<write>`.
//!
//! The pipe ends are bridged to a [`Wire`] by two plain threads (blocking reads; anonymous
//! pipes on Windows have no overlapped I/O). They end when the browser closes its end or
//! the client drops the wire.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

use genie_process::{ProcessTree, spawn_tree};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::cdp::Wire;

/// Spawn `cmd` (the browser with its other flags) in its own kill-tree with a CDP pipe.
pub fn spawn_with_pipe(cmd: &mut Command) -> io::Result<(ProcessTree, Wire)> {
    let (tree, to_browser, from_browser) = imp::spawn(cmd)?;
    Ok((tree, bridge(to_browser, from_browser)?))
}

/// Threads between the pipe files and a [`Wire`].
pub fn bridge(to_browser: File, from_browser: File) -> io::Result<Wire> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    std::thread::Builder::new()
        .name("cdp-pipe-write".into())
        .spawn(move || {
            let mut w = io::BufWriter::new(to_browser);
            while let Some(msg) = out_rx.blocking_recv() {
                let ok = w
                    .write_all(msg.as_bytes())
                    .and_then(|()| w.write_all(&[0]))
                    .and_then(|()| w.flush());
                if let Err(e) = ok {
                    tracing::debug!(error = %e, "CDP pipe write ended");
                    break;
                }
            }
        })?;
    std::thread::Builder::new()
        .name("cdp-pipe-read".into())
        .spawn(move || {
            let mut r = BufReader::new(from_browser);
            let mut buf = Vec::with_capacity(64 * 1024);
            loop {
                buf.clear();
                match r.read_until(0, &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if buf.last() == Some(&0) {
                            buf.pop();
                        }
                        let msg = String::from_utf8_lossy(&buf).into_owned();
                        if in_tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "CDP pipe read ended");
                        break;
                    }
                }
            }
        })?;
    Ok(Wire {
        tx: out_tx,
        rx: in_rx,
    })
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::*;

    /// A pipe whose ends are close-on-exec, moved to fds ≥ 10 so they never collide with
    /// 3 and 4 in the child.
    fn pipe_high() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid two-int buffer.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both fds were just returned by pipe() and are owned by nobody else.
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        Ok((high(r)?, high(w)?))
    }

    fn high(fd: OwnedFd) -> io::Result<OwnedFd> {
        // SAFETY: plain fcntl on an fd we own.
        let n = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `n` is a fresh fd from F_DUPFD_CLOEXEC.
        Ok(unsafe { OwnedFd::from_raw_fd(n) })
    }

    pub(super) fn spawn(cmd: &mut Command) -> io::Result<(genie_process::ProcessTree, File, File)> {
        let (child_in, ours_out) = pipe_high()?; // browser reads fd 3
        let (ours_in, child_out) = pipe_high()?; // browser writes fd 4
        let (ci, co) = (child_in.as_raw_fd(), child_out.as_raw_fd());
        cmd.arg("--remote-debugging-pipe");
        // SAFETY: only async-signal-safe calls (dup2) between fork and exec. dup2 clears
        // close-on-exec on the new descriptors, so the child keeps exactly 3 and 4.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(ci, 3) < 0 || libc::dup2(co, 4) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let tree = spawn_tree(cmd)?;
        drop(child_in);
        drop(child_out);
        Ok((tree, File::from(ours_out), File::from(ours_in)))
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};

    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    use super::*;

    /// An inheritable pipe; returns (read, write).
    fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut r: HANDLE = std::ptr::null_mut();
        let mut w: HANDLE = std::ptr::null_mut();
        // SAFETY: out-pointers are valid; `sa` lives across the call.
        if unsafe { CreatePipe(&mut r, &mut w, &sa, 1 << 20) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fresh handles owned by nobody else.
        Ok(unsafe {
            (
                OwnedHandle::from_raw_handle(r as RawHandle),
                OwnedHandle::from_raw_handle(w as RawHandle),
            )
        })
    }

    fn not_inherited(h: &OwnedHandle) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: plain call on a handle we own.
        if unsafe { SetHandleInformation(h.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn spawn(cmd: &mut Command) -> io::Result<(genie_process::ProcessTree, File, File)> {
        use std::os::windows::io::AsRawHandle;
        let (child_in, ours_out) = pipe()?;
        let (ours_in, child_out) = pipe()?;
        not_inherited(&ours_out)?;
        not_inherited(&ours_in)?;
        cmd.arg("--remote-debugging-pipe");
        cmd.arg(format!(
            "--remote-debugging-io-pipes={},{}",
            child_in.as_raw_handle() as usize,
            child_out.as_raw_handle() as usize
        ));
        // Known gap: inheritable handles can also leak into another child spawned at the
        // same moment (std has no PROC_THREAD_ATTRIBUTE_HANDLE_LIST support).
        let tree = spawn_tree(cmd)?;
        drop(child_in);
        drop(child_out);
        Ok((tree, File::from(ours_out), File::from(ours_in)))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_frames_on_nul() {
        use std::os::fd::{FromRawFd, OwnedFd};
        let mut a = [0i32; 2];
        let mut b = [0i32; 2];
        unsafe {
            assert_eq!(libc::pipe(a.as_mut_ptr()), 0);
            assert_eq!(libc::pipe(b.as_mut_ptr()), 0);
        }
        let (a_r, a_w, b_r, b_w) = unsafe {
            (
                File::from(OwnedFd::from_raw_fd(a[0])),
                File::from(OwnedFd::from_raw_fd(a[1])),
                File::from(OwnedFd::from_raw_fd(b[0])),
                File::from(OwnedFd::from_raw_fd(b[1])),
            )
        };
        // We write to a_w, read from b_r; the "browser" echoes a_r → b_w.
        let mut wire = bridge(a_w, b_r).unwrap();
        std::thread::spawn(move || {
            let mut r = BufReader::new(a_r);
            let mut w = b_w;
            let mut buf = vec![];
            while r.read_until(0, &mut buf).unwrap() > 0 {
                w.write_all(&buf).unwrap();
                buf.clear();
            }
        });
        wire.tx.send("{\"id\":1}".into()).unwrap();
        wire.tx.send("{\"id\":2}".into()).unwrap();
        assert_eq!(wire.rx.recv().await.unwrap(), "{\"id\":1}");
        assert_eq!(wire.rx.recv().await.unwrap(), "{\"id\":2}");
    }
}
