#!/usr/bin/env bun
// `bun tauri <args>` from the workspace root: runs the Tauri CLI in apps/desktop.
//   - `dev` first runs scripts/prep.ts (artifact runtime + genie daemon binary), so a fresh
//     checkout only needs `bun i && bun start`.
//   - `build` first builds the `genie` daemon in release mode and bundles it with the app as a
//     Tauri sidecar (src-tauri/tauri.sidecar.conf.json), so it is installed next to
//     genie-desktop(.exe) where host.rs `daemon_exe` looks for it (D40).
//   - On Linux, `dev`/`build` get `--features linux-webview` (the Tauri shell is behind that
//     feature there; see README "Build and test") unless --features is already given.
import { copyFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const app = resolve(root, "apps/desktop");
const args = process.argv.slice(2);
const sub = args[0];

function run(cmd: string[], cwd: string): number {
  return Bun.spawnSync(cmd, { cwd, stdio: ["inherit", "inherit", "inherit"] }).exitCode ?? 1;
}

function must(cmd: string[], cwd = root) {
  console.log(`\n$ ${cmd.join(" ")}`);
  const code = run(cmd, cwd);
  if (code !== 0) process.exit(code);
}

/** Value of `--name V`, `--name=V` or `-s V` in args. */
function flag(long: string, short?: string): string | undefined {
  for (let i = 0; i < args.length; i++) {
    const a = args[i]!;
    if (a === long || (short && a === short)) return args[i + 1];
    if (a.startsWith(`${long}=`)) return a.slice(long.length + 1);
  }
  return undefined;
}

function hostTriple(): string {
  const out = Bun.spawnSync(["rustc", "-vV"]).stdout.toString();
  const host = out.match(/^host: (\S+)/m)?.[1];
  if (!host) {
    console.error("`rustc -vV` failed. Install Rust (https://rustup.rs), then re-run.");
    process.exit(1);
  }
  return host;
}

// The dev server port is fixed (vite.config.ts `strictPort`, tauri.conf.json `devUrl`).
async function portInUse(port: number): Promise<boolean> {
  const { createConnection } = await import("node:net");
  const probe = (host: string) =>
    new Promise<boolean>((done) => {
      const s = createConnection({ host, port });
      s.setTimeout(500);
      s.once("connect", () => (s.destroy(), done(true)));
      s.once("error", () => done(false));
      s.once("timeout", () => (s.destroy(), done(false)));
    });
  return (await probe("127.0.0.1")) || (await probe("::1"));
}

if (sub === "dev") {
  if (await portInUse(5173)) {
    console.error(
      "Port 5173 is already in use — another dev server (an earlier `tauri dev` or `bun run web`?)\n" +
        "is still running. Stop it, then re-run. To find it: lsof -nP -iTCP:5173 -sTCP:LISTEN",
    );
    process.exit(1);
  }
  must([process.execPath, "scripts/prep.ts"]);
}

if (sub === "build") {
  // Tauri wants the sidecar at src-tauri/binaries/genie-<target triple>[.exe].
  const target = flag("--target", "-t");
  if (target === "universal-apple-darwin") {
    console.error("universal-apple-darwin: use `cargo xtask dist --target universal-apple-darwin --desktop`.");
    process.exit(1);
  }
  const triple = target ?? hostTriple();
  const debug = args.includes("--debug") || args.includes("-d");
  const profile = debug ? "debug" : "release";
  const exe = triple.includes("windows") ? ".exe" : "";
  console.log(`Building the genie daemon for ${triple} (${profile})…`);
  must(["cargo", "build", "-p", "genie-cli", ...(debug ? [] : ["--release"]), ...(target ? ["--target", target] : [])]);
  const built = resolve(root, "target", ...(target ? [target] : []), profile, `genie${exe}`);
  const binDir = resolve(app, "src-tauri/binaries");
  mkdirSync(binDir, { recursive: true });
  copyFileSync(built, resolve(binDir, `genie-${triple}${exe}`));
  args.push("--config", "src-tauri/tauri.sidecar.conf.json");
}

if (
  process.platform === "linux" &&
  (sub === "dev" || sub === "build") &&
  !args.some((a) => a === "-f" || a.startsWith("--features"))
) {
  args.push("--features", "linux-webview");
}

process.exit(run([process.execPath, "run", "tauri", ...args], app));
