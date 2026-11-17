//! Socket / pipe listener with peer identity checks (01 §1.2).

use std::io;
use std::sync::Arc;

use genie_client::Address;
use genie_core::TaskScope;

use crate::server::{DaemonState, serve_stream};

#[cfg(unix)]
pub struct Listener(tokio::net::UnixListener);

#[cfg(windows)]
pub struct Listener {
    name: String,
    security: genie_client::win::UserOnlySecurity,
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
fn create_pipe(
    name: &str,
    security: &genie_client::win::UserOnlySecurity,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    // SAFETY: `security` outlives the call and points to a valid SECURITY_ATTRIBUTES.
    unsafe {
        ServerOptions::new()
            // FILE_FLAG_FIRST_PIPE_INSTANCE: fail if someone else already owns the name
            // (pipe squatting).
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, security.as_ptr())
    }
}

pub fn bind(addr: &Address) -> io::Result<Listener> {
    match addr {
        #[cfg(unix)]
        Address::Unix(path) => {
            // We hold daemon.lock, so any existing socket file is stale.
            let _ = std::fs::remove_file(path);
            let l = tokio::net::UnixListener::bind(path)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener(l))
        }
        #[cfg(windows)]
        Address::Pipe(name) => {
            let security = genie_client::win::UserOnlySecurity::new()?;
            let next = create_pipe(name, &security, true)?;
            Ok(Listener {
                name: name.clone(),
                security,
                next,
            })
        }
        #[allow(unreachable_patterns)]
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "address kind not supported on this OS",
        )),
    }
}

pub async fn accept_loop(mut listener: Listener, state: Arc<DaemonState>, scope: TaskScope) {
    loop {
        let accepted = tokio::select! {
            r = accept(&mut listener) => r,
            () = scope.token().cancelled() => break,
        };
        match accepted {
            Ok(stream) => {
                let st = state.clone();
                let conn_scope = scope.child("connection");
                scope.spawn(async move { serve_stream(st, stream, conn_scope).await });
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                tracing::warn!(error = %e, "refused connection from another user");
            }
            Err(e) => {
                tracing::error!(error = %e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(unix)]
async fn accept(l: &mut Listener) -> io::Result<genie_client::framing::BoxStream> {
    let (s, _) = l.0.accept().await?;
    let cred = s.peer_cred()?;
    // SAFETY: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    if cred.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {}", cred.uid()),
        ));
    }
    Ok(Box::new(s))
}

#[cfg(windows)]
async fn accept(l: &mut Listener) -> io::Result<genie_client::framing::BoxStream> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;

    l.next.connect().await?;
    let fresh = create_pipe(&l.name, &l.security, false)?;
    let conn = std::mem::replace(&mut l.next, fresh);

    let mut pid = 0u32;
    // SAFETY: valid, connected pipe handle and out-pointer.
    if unsafe { GetNamedPipeClientProcessId(conn.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let peer = genie_client::win::process_user_sid(pid)?;
    let me = genie_client::win::current_user_sid()?;
    if peer != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer pid {pid}"),
        ));
    }
    Ok(Box::new(conn))
}

pub fn cleanup(addr: &Address) {
    #[cfg(unix)]
    if let Address::Unix(p) = addr {
        let _ = std::fs::remove_file(p);
    }
    #[cfg(windows)]
    let _ = addr;
}
