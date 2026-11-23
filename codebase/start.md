# Getting started with Genie desktop

Genie desktop is a [Tauri 2](https://tauri.app) app. The screens are built with Vite and
React (`apps/desktop`). Everything else runs in the `genie` daemon (`crates/`, built from
`apps/cli`), which the app starts on your machine. There's no server to set up. Models
come from the providers you connect in **Settings › Providers & Models**.

This guide covers setting up, running and building the app on **Windows** and **macOS**.
The project uses **Bun** for packages and scripts. Run every command from `codebase/`.

## 1. Install the tools (once)

You need Rust, Bun and your platform's build tools.

### Windows (10 version 1809 or later, or 11)

1. **Microsoft C++ Build Tools.** Install
   [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   and tick **Desktop development with C++**.
2. **WebView2.** It's already installed on Windows 10 and 11. If it's missing, get the
   [Evergreen runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
3. **Rust.** Run [rustup-init.exe](https://rustup.rs) and accept the default (MSVC) toolchain.
4. **Bun.** Run this in PowerShell:

   ```powershell
   powershell -c "irm bun.sh/install.ps1 | iex"
   ```

5. **OpenSSL.** See [Windows: OpenSSL](#windows-openssl) below. Without it, the first
   build stops with `Command 'perl' not found`.

### macOS

1. **Xcode Command Line Tools:** `xcode-select --install`
2. **Rust:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Bun:** `curl -fsSL https://bun.sh/install | bash` (or `brew install oven-sh/bun/bun`)

Open a new terminal after installing, then check that everything is there:

```sh
bun --version     # 1.3 or later
cargo --version
```

### Windows: OpenSSL

Genie's local database is encrypted with SQLCipher, which needs OpenSSL. By default the build
compiles OpenSSL from source, and on Windows that needs Perl. Pick **one** of these.

**A. Strawberry Perl (simplest).**

```powershell
winget install StrawberryPerl.StrawberryPerl
```

Open a new terminal and check that `perl -v` works. Git for Windows' Perl doesn't work for this.

**B. A prebuilt OpenSSL from Visual Studio's vcpkg (no Perl on your PATH).** Visual Studio
Build Tools 2022 includes vcpkg. Build OpenSSL with it once. This takes about 10 minutes, and
vcpkg downloads its own Perl for the job.

```powershell
$vcpkg = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\vcpkg\vcpkg.exe"
mkdir C:\vcpkg-openssl; cd C:\vcpkg-openssl
'{ "dependencies": [ "openssl" ] }' | Out-File -Encoding ascii vcpkg.json
& $vcpkg x-update-baseline --add-initial-baseline
& $vcpkg install --triplet x64-windows-static-md --x-install-root=C:\vcpkg-openssl\installed
```

Then tell Cargo to use it. Create `%USERPROFILE%\.cargo\config.toml` with:

```toml
[env]
OPENSSL_NO_VENDOR = "1"
OPENSSL_DIR = "C:\\vcpkg-openssl\\installed\\x64-windows-static-md"
OPENSSL_STATIC = "1"
```

Every Cargo build reads this file, from any terminal. If you delete `C:\vcpkg-openssl`
later, remove these lines too.

## 2. Install packages

```sh
bun install
```

`bun install` reads `bun.lock`, so everyone gets the same package versions. Don't use
`npm`, `pnpm` or `yarn` here.

## 3. Run the app in development

```sh
bun start
```

The first run builds the React artifact runtime and the `genie` daemon, and compiles the
Rust workspace. That takes several minutes. Later runs start in seconds. Vite serves the
screens on `http://localhost:5173`. Changes in `apps/desktop/src` reload in the window
straight away.

**Just the screens, in a browser:** `bun run web` runs the UI against an in-memory mock
engine, with no Rust needed.

## 4. Connect a model

Open **Settings › Providers & Models** and connect a provider.

- **Azure AI Foundry** doesn't list models automatically. Expand the Foundry row and add
  each deployment under **Deployments**. Enter the exact deployment name from the
  Foundry portal, plus its base model (for example `gpt-5.2`) so Genie knows its limits.

## 5. Build an installer

Each installer has to be built on its own OS.

### Windows: `setup.exe`

```powershell
bun install
bun run build:app
```

The installer is written to `target\release\bundle\nsis\Genie_0.1.0_x64-setup.exe`. It
installs for the current user, with no admin prompt, into `%LOCALAPPDATA%\Genie`. That's
the same folder that holds Genie's data (`stable\`, `dev\`). Uninstalling removes only the
program files and keeps your data. If WebView2 is missing, setup downloads it. For a silent
install, run `Genie_0.1.0_x64-setup.exe /S`.

The first build on a machine downloads NSIS from GitHub. If that fails with
`timeout: global`, see [Troubleshooting](#7-troubleshooting).

The installer is unsigned, so SmartScreen warns the first time it runs. Choose
**More info › Run anyway**.

### macOS: `.app` and `.dmg`

```sh
bun install
bun run build:app
```

The output goes to `target/release/bundle/macos/Genie.app` and `target/release/bundle/dmg/`.
Unsigned builds open with right-click › **Open**, or run `xattr -cr Genie.app`.

### Release builds

`bun run build:app` makes one installer for testing. Signed release builds, including the
Windows MSI for managed deployment, go through `cargo xtask dist --desktop` and
`cargo xtask sign`. See [release/README.md](release/README.md).

## 6. Useful commands

| Command | What it does |
|---|---|
| `bun install` | Installs packages from `bun.lock` |
| `bun start` | Runs the desktop app with live reload |
| `bun run web` | Runs only the screens in a browser, against a mock engine |
| `bun run build:app` | Builds the installer for this OS |
| `bun tauri build --bundles msi` | Windows MSI instead of the setup.exe |
| `bun run prep` | Builds just the artifact runtime and the daemon |
| `bun run typecheck` / `bun run test` | Type-checks and tests the TypeScript packages |
| `cargo test --workspace` | Runs the Rust tests |
| `bun tauri icon path/to/logo.png` | Regenerates every app icon size (512×512 PNG or larger) |

## 7. Troubleshooting

**`Command 'perl' not found` / `openssl-src: failed to build OpenSSL from source`.** See
[Windows: OpenSSL](#windows-openssl). `bun run build:app` checks for this before it builds.

**`failed to bundle project: timeout: global` after `Downloading ...nsis-3.13.zip`.** Tauri's
downloader can time out on networks where `curl` works fine. Download the two files yourself
and put them where Tauri looks:

```powershell
$t = "$env:LOCALAPPDATA\tauri"; mkdir $t -Force | Out-Null; cd $env:TEMP
curl.exe -LO https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.13/nsis-3.13.zip
curl.exe -LO https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll
Expand-Archive nsis-3.13.zip $env:TEMP\nsis-x; Move-Item $env:TEMP\nsis-x\nsis-3.13 $t\NSIS
mkdir $t\NSIS\Plugins\x86-unicode\additional -Force | Out-Null
Copy-Item nsis_tauri_utils.dll $t\NSIS\Plugins\x86-unicode\additional\
```

Then run `bun run build:app` again. These versions match Tauri CLI 2.12. A newer CLI may want
newer ones, and the URL is in the error message.

**`Port 5173 is already in use`.** Another `bun start` or `bun run web` is still running.
Stop it, or find it with `netstat -ano | findstr 5173` (Windows) or `lsof -i :5173` (macOS).

**`cargo` or `bun` not found.** Open a new terminal after installing them. On Windows, check
that `%USERPROFILE%\.cargo\bin` and `%USERPROFILE%\.bun\bin` are on your `PATH`.

**Windows: `link.exe not found` or MSVC errors.** The C++ Build Tools aren't installed, or the
**Desktop development with C++** workload wasn't selected. Rerun the Visual Studio Installer
and add it.

**Slow builds or locked files in OneDrive.** If the repo is in a OneDrive folder, OneDrive
also syncs `target/`, which can be gigabytes. Clone the repo outside OneDrive (for example
`C:\src\genie`). Don't set `CARGO_TARGET_DIR`: `bun start` and `bun run build:app` expect the
daemon in `codebase/target/`.

**Packages look wrong after switching branches.** Run `bun install` again. If that doesn't
help, delete `node_modules` and run `bun install`.
