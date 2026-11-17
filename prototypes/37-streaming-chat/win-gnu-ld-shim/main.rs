//! `ld.exe` shim for stable-x86_64-pc-windows-gnu without MinGW (prototype 37 workaround).
//! The toolchain's gcc driver runs `ld` from its -B dir. We forward to rust-lld (GNU flavor; `-m i386pep`
//! selects its MinGW driver). gcc writes @response files with GNU quoting (spaces escaped by backslashes)
//! but lld on Windows reads them with Windows quoting, which breaks paths containing spaces, so we
//! re-encode every @file with Windows quoting first.
//! Build: rustc +stable-x86_64-pc-windows-gnu -O -o .tools/lld-as-ld/ld.exe win-gnu-ld-shim/main.rs
use std::process::{exit, Command};

fn parse_gnu(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut have) = (Vec::new(), String::new(), false);
    let mut quote: Option<char> = None;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (quote, c) {
            (_, '\\') => {
                if let Some(n) = it.next() {
                    cur.push(n);
                    have = true;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                have = true;
            }
            (Some(q), _) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            _ => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// MSVC/Windows command-line quoting: backslashes are literal unless they precede a quote.
fn quote_win(a: &str) -> String {
    let mut s = String::from("\"");
    let mut bs = 0;
    for c in a.chars() {
        if c == '\\' {
            bs += 1;
            continue;
        }
        if c == '"' {
            s.push_str(&"\\".repeat(bs * 2 + 1));
        } else {
            s.push_str(&"\\".repeat(bs));
        }
        bs = 0;
        s.push(c);
    }
    s.push_str(&"\\".repeat(bs * 2));
    s.push('"');
    s
}

fn main() {
    let lld = std::env::current_exe().unwrap().with_file_name("rust-lld.exe");
    let mut args: Vec<String> = vec!["-flavor".into(), "gnu".into()];
    let mut temps = Vec::new();
    for (i, a) in std::env::args().skip(1).enumerate() {
        if let Some(path) = a.strip_prefix('@') {
            if let Ok(text) = std::fs::read_to_string(path) {
                let body: Vec<String> = parse_gnu(&text).iter().map(|x| quote_win(x)).collect();
                let new = std::env::temp_dir().join(format!("ld-shim-{}-{i}.rsp", std::process::id()));
                std::fs::write(&new, body.join("\n")).unwrap();
                args.push(format!("@{}", new.display()));
                temps.push(new);
                continue;
            }
        }
        args.push(a);
    }
    let status = Command::new(lld).args(&args).status().expect("failed to run rust-lld.exe");
    for t in temps {
        let _ = std::fs::remove_file(t);
    }
    exit(status.code().unwrap_or(1));
}
