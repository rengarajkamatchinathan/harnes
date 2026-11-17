//! `cargo xtask <check|check-deps|check-spawn|schema>`: architecture checks (01 §2.1) and
//! protocol schema/TypeScript codegen with drift and compatibility checks (02 §11).

mod deps;
mod schema;
mod spawn;
mod ts;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "check".into());
    let root = workspace_root();
    let results = match cmd.as_str() {
        "check" => vec![
            deps::check(&root),
            spawn::check(&root),
            schema::run(&root, "check"),
        ],
        "schema" => {
            let mode = match std::env::args().nth(2).as_deref() {
                Some("--check") => "check",
                Some("--release") => "release",
                _ => "write",
            };
            vec![schema::run(&root, mode)]
        }
        "check-deps" => vec![deps::check(&root)],
        "check-spawn" => vec![spawn::check(&root)],
        other => {
            eprintln!(
                "unknown command `{other}`; expected check | check-deps | check-spawn | schema [--check|--release]"
            );
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
