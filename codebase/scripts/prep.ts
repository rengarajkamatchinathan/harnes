#!/usr/bin/env bun
// Gets the workspace ready to run the desktop app. Safe to run repeatedly: each step is
// skipped (or incremental) when its output is already up to date.
//
//   1. packages/artifact-runtime/dist  (React artifact runtime, 05 §8) — rebuilt when missing
//      or older than its sources.
//   2. target/debug/genie              (the daemon binary) — `cargo build -p genie-cli`, so
//      `tauri dev` finds it next to target/debug/genie-desktop (host.rs `daemon_exe`).
//      Skipped with --frontend-only or when GENIE_DAEMON_PATH is set.
//
// Usage: bun scripts/prep.ts [--frontend-only]
import { existsSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const frontendOnly = process.argv.includes("--frontend-only");

function run(cmd: string[], cwd = root) {
  console.log(`\n$ ${cmd.join(" ")}`);
  const { exitCode } = Bun.spawnSync(cmd, { cwd, stdio: ["inherit", "inherit", "inherit"] });
  if (exitCode !== 0) process.exit(exitCode ?? 1);
}

function newestMtime(path: string): number {
  const st = statSync(path);
  if (!st.isDirectory()) return st.mtimeMs;
  let max = st.mtimeMs;
  for (const entry of readdirSync(path)) max = Math.max(max, newestMtime(join(path, entry)));
  return max;
}

// 1. Artifact runtime
const runtime = join(root, "packages/artifact-runtime");
const manifest = join(runtime, "dist/manifest.json");
const sources = ["src", "build.mjs", "package.json"].map((p) => join(runtime, p));
const stale =
  !existsSync(manifest) || Math.max(...sources.map(newestMtime)) > statSync(manifest).mtimeMs;
if (stale) {
  console.log("Building the React artifact runtime (packages/artifact-runtime/dist)…");
  run([process.execPath, "run", "build"], runtime);
} else {
  console.log("Artifact runtime is up to date.");
}

// 2. Daemon binary
if (!frontendOnly) {
  if (process.env.GENIE_DAEMON_PATH) {
    console.log(`Using GENIE_DAEMON_PATH=${process.env.GENIE_DAEMON_PATH}`);
  } else {
    if (Bun.which("cargo") === null) {
      console.error(
        "\n`cargo` was not found. Install Rust (https://rustup.rs), then re-run.\n" +
          "To only run the UI in a browser against the mock engine: bun run web",
      );
      process.exit(1);
    }
    console.log("Building the genie daemon (cargo build -p genie-cli)…");
    run(["cargo", "build", "-p", "genie-cli"]);
  }
}
