//! Which shell runs a command (06 §13.2).
//!
//! - **Windows:** `pwsh` (PowerShell 7) if it is on `PATH`, else Windows PowerShell 5.1;
//!   `shell: "bash"` uses Git Bash when installed (never WSL's `System32\bash.exe`).
//! - **macOS / Linux:** the user's login shell (`$SHELL`), run non-interactively: zsh (with
//!   `+o nomatch`) or bash; any other login shell (fish, …) falls back to bash, then `sh`.
//!   `shell: "powershell"` uses `pwsh` when installed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::scan::Dialect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    /// A plain POSIX `sh` (fallback).
    Sh,
    /// PowerShell 7+.
    Pwsh,
    /// Windows PowerShell 5.1.
    WindowsPowerShell,
    GitBash,
}

impl ShellKind {
    pub fn dialect(self) -> Dialect {
        match self {
            ShellKind::Pwsh | ShellKind::WindowsPowerShell => Dialect::PowerShell,
            _ => Dialect::Posix,
        }
    }

    /// Named in the tool description and the `<env>` block.
    pub fn display_name(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::Zsh => "zsh",
            ShellKind::Sh => "sh",
            ShellKind::Pwsh => "PowerShell 7 (pwsh)",
            ShellKind::WindowsPowerShell => "Windows PowerShell 5.1",
            ShellKind::GitBash => "Git Bash",
        }
    }
}

/// The shell a call runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChoice {
    pub kind: ShellKind,
    pub program: PathBuf,
}

/// What the model asked for (`shell` input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Requested {
    #[default]
    Default,
    Bash,
    PowerShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Unix,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) { Os::Windows } else { Os::Unix }
    }
}

/// What selection looks at; [`Probe::system`] reads the real machine, tests fake it.
pub struct Probe<'a> {
    pub os: Os,
    /// `$SHELL` (Unix).
    pub login_shell: Option<PathBuf>,
    /// Finds a program on `PATH`.
    pub which: &'a dyn Fn(&str) -> Option<PathBuf>,
    pub exists: &'a dyn Fn(&Path) -> bool,
    /// Environment lookups for well-known install dirs (`ProgramFiles`, `LOCALAPPDATA`,
    /// `SystemRoot`).
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
}

impl Probe<'static> {
    pub fn system() -> Probe<'static> {
        Probe {
            os: Os::current(),
            login_shell: std::env::var_os("SHELL").map(PathBuf::from),
            which: &which,
            exists: &is_file,
            var: &env_var,
        }
    }
}

/// Pick the shell for a call.
pub fn select(probe: &Probe<'_>, requested: Requested) -> Result<ShellChoice, String> {
    match probe.os {
        Os::Windows => match requested {
            Requested::Bash => git_bash(probe)
                .map(|program| ShellChoice {
                    kind: ShellKind::GitBash,
                    program,
                })
                .ok_or_else(|| {
                    "Git Bash is not installed; use PowerShell or install Git for Windows".into()
                }),
            Requested::Default | Requested::PowerShell => Ok(windows_powershell(probe)),
        },
        Os::Unix => match requested {
            Requested::PowerShell => (probe.which)("pwsh")
                .map(|program| ShellChoice {
                    kind: ShellKind::Pwsh,
                    program,
                })
                .ok_or_else(|| "PowerShell (pwsh) is not installed".into()),
            Requested::Bash => unix_bash(probe).ok_or_else(|| "bash is not installed".into()),
            Requested::Default => {
                if let Some(sh) = probe.login_shell.as_ref().filter(|p| (probe.exists)(p)) {
                    let name = sh
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let kind = match name.as_str() {
                        "zsh" => Some(ShellKind::Zsh),
                        "bash" => Some(ShellKind::Bash),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        return Ok(ShellChoice {
                            kind,
                            program: sh.clone(),
                        });
                    }
                }
                if let Some(c) = unix_bash(probe) {
                    return Ok(c);
                }
                let sh = PathBuf::from("/bin/sh");
                if (probe.exists)(&sh) {
                    return Ok(ShellChoice {
                        kind: ShellKind::Sh,
                        program: sh,
                    });
                }
                Err("no POSIX shell found (bash or /bin/sh)".into())
            }
        },
    }
}

fn unix_bash(probe: &Probe<'_>) -> Option<ShellChoice> {
    let program = (probe.which)("bash").or_else(|| {
        let p = PathBuf::from("/bin/bash");
        (probe.exists)(&p).then_some(p)
    })?;
    Some(ShellChoice {
        kind: ShellKind::Bash,
        program,
    })
}

fn windows_powershell(probe: &Probe<'_>) -> ShellChoice {
    if let Some(program) = (probe.which)("pwsh") {
        return ShellChoice {
            kind: ShellKind::Pwsh,
            program,
        };
    }
    let program = (probe.which)("powershell").unwrap_or_else(|| {
        let root = (probe.var)("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
    });
    ShellChoice {
        kind: ShellKind::WindowsPowerShell,
        program,
    }
}

/// Git for Windows' `bin\bash.exe`: next to `git` on `PATH`, then the default install dirs.
fn git_bash(probe: &Probe<'_>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(git) = (probe.which)("git") {
        // `<root>\cmd\git.exe` or `<root>\bin\git.exe` (or `mingw64\bin`).
        for anc in git.ancestors().skip(1).take(3) {
            candidates.push(anc.join("bin").join("bash.exe"));
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(root) = (probe.var)(var) {
            candidates.push(PathBuf::from(root).join(r"Git\bin\bash.exe"));
        }
    }
    if let Some(local) = (probe.var)("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join(r"Programs\Git\bin\bash.exe"));
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
    candidates.into_iter().find(|c| (probe.exists)(c))
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

fn env_var(k: &str) -> Option<OsString> {
    std::env::var_os(k)
}

/// Search `PATH` (with `PATHEXT` on Windows).
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{program}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe<'a>(
        os: Os,
        login: Option<&str>,
        which: &'a dyn Fn(&str) -> Option<PathBuf>,
        exists: &'a dyn Fn(&Path) -> bool,
    ) -> Probe<'a> {
        Probe {
            os,
            login_shell: login.map(PathBuf::from),
            which,
            exists,
            var: &|k| match k {
                "ProgramFiles" => Some(r"C:\Program Files".into()),
                "SystemRoot" => Some(r"C:\Windows".into()),
                _ => None,
            },
        }
    }

    #[test]
    fn unix_login_shell() {
        let which = |p: &str| (p == "bash").then(|| PathBuf::from("/usr/bin/bash"));
        let exists = |_: &Path| true;
        let c = select(
            &probe(Os::Unix, Some("/bin/zsh"), &which, &exists),
            Requested::Default,
        )
        .unwrap();
        assert_eq!(c.kind, ShellKind::Zsh);
        let c = select(
            &probe(Os::Unix, Some("/usr/local/bin/bash"), &which, &exists),
            Requested::Default,
        )
        .unwrap();
        assert_eq!(
            (c.kind, c.program),
            (ShellKind::Bash, "/usr/local/bin/bash".into())
        );
        // fish → bash fallback.
        let c = select(
            &probe(Os::Unix, Some("/usr/bin/fish"), &which, &exists),
            Requested::Default,
        )
        .unwrap();
        assert_eq!(
            (c.kind, c.program),
            (ShellKind::Bash, "/usr/bin/bash".into())
        );
        // No bash at all → /bin/sh.
        let none = |_: &str| None;
        let only_sh = |p: &Path| p == Path::new("/bin/sh");
        let c = select(&probe(Os::Unix, None, &none, &only_sh), Requested::Default).unwrap();
        assert_eq!(c.kind, ShellKind::Sh);
        assert_eq!(c.kind.dialect(), Dialect::Posix);
        assert!(
            select(
                &probe(Os::Unix, None, &none, &only_sh),
                Requested::PowerShell
            )
            .is_err()
        );
    }

    #[test]
    fn windows_shells() {
        let root = PathBuf::from(if cfg!(windows) { r"D:\Git" } else { "/d/Git" });
        let git = root.join("cmd").join("git.exe");
        let bash = root.join("bin").join("bash.exe");
        let with_pwsh = |p: &str| match p {
            "pwsh" => Some(PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            "git" => Some(git.clone()),
            _ => None,
        };
        let exists = |p: &Path| p == bash;
        let pr = probe(Os::Windows, None, &with_pwsh, &exists);
        let c = select(&pr, Requested::Default).unwrap();
        assert_eq!(c.kind, ShellKind::Pwsh);
        assert_eq!(c.kind.dialect(), Dialect::PowerShell);
        let c = select(&pr, Requested::Bash).unwrap();
        assert_eq!(c.kind, ShellKind::GitBash);
        assert_eq!(c.program, bash);

        let bare = |_: &str| None;
        let nothing = |_: &Path| false;
        let pr = probe(Os::Windows, None, &bare, &nothing);
        let c = select(&pr, Requested::Default).unwrap();
        assert_eq!(c.kind, ShellKind::WindowsPowerShell);
        assert!(c.program.to_string_lossy().ends_with("powershell.exe"));
        assert!(select(&pr, Requested::Bash).is_err());
    }
}
