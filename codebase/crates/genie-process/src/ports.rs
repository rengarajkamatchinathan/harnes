//! Which TCP ports a process tree is listening on (ticket 43). The shell tool uses it to
//! spot a dev server started in the foreground and move it to the background instead of
//! blocking the turn until the timeout.
//!
//! * **macOS:** `lsof -a -g <pgid> -iTCP -sTCP:LISTEN` (about 15 ms).
//! * **Linux:** `/proc/<pid>/stat` for the group, their socket inodes from `/proc/<pid>/fd`,
//!   matched against the `LISTEN` rows of `/proc/net/tcp{,6}`.
//! * **Windows:** the Job Object's process ids, matched against `netstat -ano` rows whose
//!   remote address ends in `:0` (listening; locale-independent, unlike the state column).
//!
//! Every error reads as "no ports": detection is best effort and never fails a command.

/// A cheap handle on a process tree, usable off the tree's borrow (e.g. on a blocking
/// thread while the tree is being read).
#[derive(Debug, Clone)]
pub struct TreeProbe {
    #[cfg(unix)]
    pub(crate) pgid: i32,
    #[cfg(windows)]
    pub(crate) pids: Vec<u32>,
}

impl TreeProbe {
    /// Listening TCP ports of the tree, sorted and deduplicated. **Blocking**: call it from
    /// `spawn_blocking`. Empty when nothing listens or the platform query fails.
    pub fn listening_ports(&self) -> Vec<u16> {
        let mut ports = self.query();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    #[cfg(target_os = "macos")]
    fn query(&self) -> Vec<u16> {
        use std::process::{Command, Stdio};
        let out = Command::new("/usr/sbin/lsof")
            .args([
                "-nP",
                "-a",
                "-g",
                &self.pgid.to_string(),
                "-iTCP",
                "-sTCP:LISTEN",
                "-Fn",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match out {
            Ok(o) => parse_lsof(&String::from_utf8_lossy(&o.stdout)),
            Err(_) => vec![],
        }
    }

    #[cfg(target_os = "linux")]
    fn query(&self) -> Vec<u16> {
        let inodes = linux::group_socket_inodes(self.pgid);
        if inodes.is_empty() {
            return vec![];
        }
        ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .flat_map(|t| parse_proc_net_tcp(&t, &inodes))
            .collect()
    }

    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    fn query(&self) -> Vec<u16> {
        vec![]
    }

    #[cfg(windows)]
    fn query(&self) -> Vec<u16> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        if self.pids.is_empty() {
            return vec![];
        }
        let out = Command::new("netstat")
            .args(["-ano"])
            .creation_flags(crate::windows::CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match out {
            Ok(o) => parse_netstat(&String::from_utf8_lossy(&o.stdout), &self.pids),
            Err(_) => vec![],
        }
    }
}

/// The port of a `host:port` / `[v6]:port` address.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')?.1.parse().ok()
}

/// `lsof -F n` output: `n<addr>:<port>` lines.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_lsof(out: &str) -> Vec<u16> {
    out.lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter_map(port_of)
        .collect()
}

/// `/proc/net/tcp{,6}` rows in state `0A` (LISTEN) whose inode belongs to the group.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_net_tcp(table: &str, inodes: &std::collections::HashSet<u64>) -> Vec<u16> {
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            let (local, state, inode) = (cols.get(1)?, cols.get(3)?, cols.get(9)?);
            if *state != "0A" || !inodes.contains(&inode.parse::<u64>().ok()?) {
                return None;
            }
            u16::from_str_radix(local.rsplit_once(':')?.1, 16).ok()
        })
        .collect()
}

/// `netstat -ano` TCP rows owned by `pids` whose remote address is `*:0` (listening).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat(out: &str, pids: &[u32]) -> Vec<u16> {
    out.lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            if !cols.first()?.eq_ignore_ascii_case("TCP") || cols.len() < 4 {
                return None;
            }
            let pid: u32 = cols.last()?.parse().ok()?;
            let remote = cols.get(2)?;
            if !pids.contains(&pid) || port_of(remote) != Some(0) {
                return None;
            }
            port_of(cols.get(1)?)
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashSet;

    /// Socket inodes held open by any process whose process group is `pgid`.
    pub(super) fn group_socket_inodes(pgid: i32) -> HashSet<u64> {
        let mut inodes = HashSet::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return inodes;
        };
        for p in procs.flatten() {
            let name = p.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            if pgrp_of(pid) != Some(pgid) {
                continue;
            }
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path())
                    && let Some(inode) = target
                        .to_str()
                        .and_then(|t| t.strip_prefix("socket:["))
                        .and_then(|t| t.strip_suffix(']'))
                        .and_then(|t| t.parse().ok())
                {
                    inodes.insert(inode);
                }
            }
        }
        inodes
    }

    /// Field 5 of `/proc/<pid>/stat`, read after the `)` that ends the command name.
    fn pgrp_of(pid: &str) -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(2)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsof_output() {
        let out = "p7997\nf14\nn127.0.0.1:3000\np8207\nf26\nn[::1]:1420\nn*:3000\n";
        assert_eq!(parse_lsof(out), [3000, 1420, 3000]);
    }

    #[test]
    fn proc_net_tcp_listen_rows_of_the_group() {
        let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0\n   1: 0100007F:0BB9 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 9999 1 0\n   2: 0100007F:D431 0100007F:0BB8 01 00000000:00000000 00:00000000 00000000  1000        0 4243 1 0\n";
        let mine: std::collections::HashSet<u64> = [4242, 4243].into();
        assert_eq!(parse_proc_net_tcp(t, &mine), [3000]);
    }

    #[test]
    fn netstat_listening_rows_of_the_tree() {
        let out = "\nActive Connections\n\n  Proto  Local Address          Foreign Address        State           PID\n  TCP    0.0.0.0:3000           0.0.0.0:0              LISTENING       40\n  TCP    [::1]:1420             [::]:0                 LISTENING       41\n  TCP    127.0.0.1:50000        127.0.0.1:3000         ESTABLISHED     40\n  TCP    0.0.0.0:445            0.0.0.0:0              LISTENING       4\n  UDP    0.0.0.0:5353           *:*                                    40\n";
        assert_eq!(parse_netstat(out, &[40, 41]), [3000, 1420]);
    }
}
