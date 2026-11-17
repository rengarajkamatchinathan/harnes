//! `cargo xtask <command>`: architecture checks (01 §2.1), protocol schema/TypeScript
//! codegen with drift and compatibility checks (02 §11), CI steps, release artifacts and
//! signed feeds (09). Workflows call these commands so the CI platform stays swappable.

mod checks;
mod ci;
mod deps;
mod dist;
mod feed;
mod feedbuild;
mod opts;
mod schema;
mod spawn;
mod timefmt;
mod ts;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "expected one of:
  check                      check-deps + check-spawn + schema --check
  check-deps | check-spawn | check-unsafe | check-headers | secret-scan
  schema [--check|--release]
  ci [--list | <step>...]    what PR CI runs (09 §5.1)
  nightly <suite>            main/nightly suites (09 §5.2)
  dist [--target T] [--channel C] [--out DIR] [--base-url URL] [--no-build] [--desktop]
  sbom | notices | checksums [--out DIR]
  sign <macos|windows|linux> <files...>
  feed <build|keygen|pubkey|stamp|sign|sign-file|verify|rollout|gate|bucket> ...";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "check".into());
    let rest: Vec<String> = args.collect();
    let root = workspace_root();
    let results = match cmd.as_str() {
        "check" => vec![
            deps::check(&root),
            spawn::check(&root),
            schema::run(&root, "check"),
        ],
        "schema" => {
            let mode = match rest.first().map(String::as_str) {
                Some("--check") => "check",
                Some("--release") => "release",
                _ => "write",
            };
            vec![schema::run(&root, mode)]
        }
        "check-deps" => vec![deps::check(&root)],
        "check-spawn" => vec![spawn::check(&root)],
        "check-unsafe" => vec![checks::check_unsafe(&root)],
        "check-headers" => vec![checks::check_headers(&root)],
        "secret-scan" => vec![checks::secret_scan(&root)],
        "ci" => vec![ci::run(&root, &rest)],
        "nightly" => vec![ci::nightly(&root, &rest)],
        "dist" | "sbom" | "notices" | "checksums" | "sign" => vec![dist::run(&root, &cmd, rest)],
        "feed" => vec![feed::run(&root, rest)],
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown command `{other}`; {USAGE}");
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
