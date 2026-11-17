//! `cargo xtask ci [step…]`: what PR CI runs (09 §5.1), runnable locally. The workflows in
//! `.github/workflows/` call these steps one by one so the CI platform stays swappable.
//!
//! Steps: fmt, clippy, test, check (deps + spawn + schema), unsafe, headers, secrets, feeds,
//! deny, frontend. With no step, all run in that order and every failure is reported at the end.
//! Tools that aren't installed (cargo-deny, pnpm) are skipped locally and are errors when
//! `CI` is set (CI must not silently skip a gate).

use std::path::Path;
use std::process::Command;

use crate::{checks, deps, schema, spawn};

pub const STEPS: &[(&str, &str)] = &[
    ("fmt", "cargo fmt --all --check"),
    (
        "clippy",
        "cargo clippy --workspace --all-targets --locked -D warnings",
    ),
    ("test", "cargo test --workspace --locked"),
    (
        "check",
        "crate graph rules, no stray tokio::spawn, protocol schema drift + compatibility",
    ),
    (
        "unsafe",
        "unsafe only where 08 §9 allows, each with a SAFETY note",
    ),
    ("headers", "OpenCode-derived files carry the MIT notice"),
    (
        "secrets",
        "no credentials in tracked files, fixtures or cassettes",
    ),
    (
        "feeds",
        "feed sources build, sign (throwaway keys) and verify",
    ),
    (
        "deny",
        "cargo deny check (licences, advisories, bans, sources)",
    ),
    (
        "frontend",
        "artifact runtime build, pnpm typecheck/lint/test, desktop vite build, pnpm audit",
    ),
];

pub fn run(root: &Path, args: &[String]) -> Result<String, Vec<String>> {
    if args.iter().any(|a| a == "--list") {
        let lines: Vec<String> = STEPS.iter().map(|(n, d)| format!("{n:9} {d}")).collect();
        return Ok(format!("ci steps:\n{}", lines.join("\n")));
    }
    let wanted: Vec<&str> = if args.is_empty() {
        STEPS.iter().map(|(n, _)| *n).collect()
    } else {
        args.iter().map(String::as_str).collect()
    };
    let mut errors = vec![];
    let mut done = vec![];
    for step in wanted {
        eprintln!("==> ci {step}");
        let r = run_step(root, step);
        match r {
            Ok(msg) => {
                println!("ok: {msg}");
                done.push(step);
            }
            Err(errs) => {
                for e in &errs {
                    eprintln!("error: {e}");
                }
                errors.push(format!("ci step `{step}` failed"));
            }
        }
    }
    if errors.is_empty() {
        Ok(format!("ci: {} passed", done.join(", ")))
    } else {
        Err(errors)
    }
}

fn in_ci() -> bool {
    std::env::var_os("CI").is_some_and(|v| !v.is_empty() && v != "false")
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn run_step(root: &Path, step: &str) -> Result<String, Vec<String>> {
    match step {
        "fmt" => cmd(root, &cargo(), &["fmt", "--all", "--check"]),
        "clippy" => cmd(
            root,
            &cargo(),
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "test" => cmd(root, &cargo(), &["test", "--workspace", "--locked"]),
        "check" => {
            let mut errs = vec![];
            let mut oks = vec![];
            for r in [
                deps::check(root),
                spawn::check(root),
                schema::run(root, "check"),
            ] {
                match r {
                    Ok(m) => oks.push(m),
                    Err(e) => errs.extend(e),
                }
            }
            if errs.is_empty() {
                Ok(oks.join("; "))
            } else {
                Err(errs)
            }
        }
        "unsafe" => checks::check_unsafe(root),
        "headers" => checks::check_headers(root),
        "secrets" => checks::secret_scan(root),
        "feeds" => crate::feedbuild::dry_run(root),
        "deny" => {
            if !tool_exists(&cargo(), &["deny", "--version"]) {
                return missing("cargo-deny", "cargo install cargo-deny --locked");
            }
            cmd(root, &cargo(), &["deny", "--log-level", "error", "check"])
        }
        "frontend" => frontend(root),
        other => Err(vec![format!(
            "unknown ci step `{other}`; see `cargo xtask ci --list`"
        )]),
    }
}

fn frontend(root: &Path) -> Result<String, Vec<String>> {
    if !root.join("pnpm-workspace.yaml").is_file() {
        return Ok(
            "frontend: no pnpm workspace yet (apps/desktop, packages/ui pending), skipped".into(),
        );
    }
    if !tool_exists("pnpm", &["--version"]) {
        return missing("pnpm", "corepack enable");
    }
    cmd(root, "pnpm", &["install", "--frozen-lockfile"])?;
    // The React artifact runtime (05 §8) is built first: its tests and the desktop
    // bundle use packages/artifact-runtime/dist.
    cmd(
        root,
        "pnpm",
        &["--filter", "@genie/artifact-runtime", "run", "build"],
    )?;
    for script in ["typecheck", "lint", "test"] {
        cmd(root, "pnpm", &["-r", "--if-present", script])?;
    }
    cmd(
        root,
        "pnpm",
        &["--filter", "@genie/desktop", "exec", "vite", "build"],
    )?;
    cmd(root, "pnpm", &["audit", "--prod", "--audit-level", "high"])?;
    Ok("frontend: artifact runtime, typecheck, lint, test, vite build, audit".into())
}

fn missing(tool: &str, install: &str) -> Result<String, Vec<String>> {
    if in_ci() {
        Err(vec![format!("{tool} is not installed ({install})")])
    } else {
        eprintln!("warning: {tool} not installed, step skipped locally ({install})");
        Ok(format!("{tool} missing, skipped (would fail with CI set)"))
    }
}

fn tool_exists(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn cmd(root: &Path, prog: &str, args: &[&str]) -> Result<String, Vec<String>> {
    let shown = format!("{prog} {}", args.join(" "));
    let status = Command::new(prog)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| vec![format!("{shown}: {e}")])?;
    if status.success() {
        Ok(shown)
    } else {
        Err(vec![format!("`{shown}` exited with {status}")])
    }
}

/// `cargo xtask nightly <suite>` (09 §5.2). Suites that need code which doesn't exist yet
/// fail loudly instead of passing empty; the workflow only schedules them once their
/// prerequisite exists (`apps/desktop`, `fuzz/`, the live-smoke repository variable).
pub fn nightly(root: &Path, args: &[String]) -> Result<String, Vec<String>> {
    let suite = args.first().map(String::as_str).unwrap_or("");
    let c = cargo();
    match suite {
        // Cross-OS daemon tests: socket/pipe security, ensure race, version skew, crash → Resume.
        "daemon" => cmd(
            root,
            &c,
            &["test", "--locked", "-p", "genie-daemon", "-p", "genie-client", "-p", "genie-core"],
        ),
        // Shell and process tests per OS (kill-tree, pipe mode, shell tool).
        "process" => {
            cmd(root, &c, &["test", "--locked", "-p", "genie-process"])?;
            cmd(root, &c, &["test", "--locked", "-p", "genie-tools", "--test", "shell"])
        }
        "hostile-artifact" | "perf-gate" | "e2e-desktop" | "browser-redteam" | "fuzz"
        | "live-providers" => Err(vec![format!(
            "nightly suite `{suite}` is not implemented yet (09 §5.2; see README Known gaps)"
        )]),
        _ => Err(vec![
            "expected nightly <daemon|process|hostile-artifact|perf-gate|e2e-desktop|browser-redteam|fuzz|live-providers>".into(),
        ]),
    }
}
