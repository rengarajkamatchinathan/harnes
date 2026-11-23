#!/usr/bin/env bun
// `bun run build:app`: builds the desktop installer for this OS (see start.md).
//   Windows → target\release\bundle\nsis\Genie_<version>_x64-setup.exe (per-user, no admin)
//   macOS   → target/release/bundle/macos/Genie.app and bundle/dmg/
//   Linux   → Tauri's default Linux bundles
// It wraps `bun tauri build` (scripts/tauri.ts), which builds and bundles the genie daemon.
// Extra arguments are passed through, e.g. `bun run build:app --debug`.
// Release builds with every bundle type go through `cargo xtask dist --desktop`.
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const args = process.argv.slice(2);

const bundles: Record<string, string | undefined> = {
  win32: "nsis",
  darwin: "app,dmg",
};

/** Windows: the encrypted store (rusqlite + SQLCipher) builds OpenSSL from source, which
 *  needs Strawberry Perl, unless a prebuilt OpenSSL is set via OPENSSL_NO_VENDOR. */
function checkWindowsOpenSsl() {
  if (process.platform !== "win32") return;
  if (process.env.OPENSSL_NO_VENDOR || Bun.which("perl")) return;
  const configs = [join(homedir(), ".cargo", "config.toml"), join(root, ".cargo", "config.toml")];
  if (configs.some((f) => existsSync(f) && readFileSync(f, "utf8").includes("OPENSSL_NO_VENDOR"))) return;
  console.error(
    "OpenSSL can't be built: `perl` isn't on PATH and no prebuilt OpenSSL is configured.\n" +
      "Fix one of these ways (start.md, \"Windows: OpenSSL\"):\n" +
      "  - winget install StrawberryPerl.StrawberryPerl   (then open a new terminal), or\n" +
      "  - build OpenSSL with Visual Studio's vcpkg and set OPENSSL_NO_VENDOR / OPENSSL_DIR /\n" +
      "    OPENSSL_STATIC in %USERPROFILE%\\.cargo\\config.toml under [env].",
  );
  process.exit(1);
}

checkWindowsOpenSsl();

const b = bundles[process.platform];
const extra = b && !args.some((a) => a === "-b" || a.startsWith("--bundles")) ? ["--bundles", b] : [];
const cmd = [process.execPath, "scripts/tauri.ts", "build", ...extra, ...args];
console.log(`$ bun ${cmd.slice(1).join(" ")}`);
const { exitCode } = Bun.spawnSync(cmd, { cwd: root, stdio: ["inherit", "inherit", "inherit"] });
if (exitCode === 0 && process.platform === "win32") {
  const profile = args.includes("--debug") || args.includes("-d") ? "debug" : "release";
  console.log(`\nInstaller: target\\${profile}\\bundle\\nsis\\ (unsigned: SmartScreen shows \"More info\" → \"Run anyway\")`);
}
process.exit(exitCode ?? 1);
