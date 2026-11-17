//! `cargo xtask <check|check-deps|check-spawn>`: architecture checks (01 §2.1).

mod deps;
mod spawn;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "check".into());
    let root = workspace_root();
    let results = match cmd.as_str() {
        "check" => vec![deps::check(&root), spawn::check(&root)],
        "check-deps" => vec![deps::check(&root)],
        "check-spawn" => vec![spawn::check(&root)],
        other => {
            eprintln!("unknown command `{other}`; expected check | check-deps | check-spawn");
            return ExitCode::FAILURE;
        }
    };
    let mut ok = true;
    for r in results {
        match r {
            Ok(msg) => println!("ok: {msg}"),
            Err(errs) => {
                ok = false;
                for e in errs {
                    eprintln!("error: {e}");
                }
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().map(PathBuf::from).unwrap_or(here)
}
