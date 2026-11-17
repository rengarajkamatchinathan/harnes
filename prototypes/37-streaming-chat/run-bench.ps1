# Prototype 37 bench on Windows (WebView2).
#   .\run-bench.ps1                 build (release) + bench
#   .\run-bench.ps1 -SkipBuild      bench only
#   .\run-bench.ps1 -Bundle         also build the NSIS installer to record its size
#   .\run-bench.ps1 -Coalesce 0     disable Rust-side delta coalescing (comparison run)
#   .\run-bench.ps1 -Toolchain msvc force MSVC (default: msvc if cl.exe/VS is present, else windows-gnu)
param(
  [switch]$SkipBuild,
  [switch]$Bundle,
  [int]$Coalesce = 16,
  [string]$Toolchain = "auto",
  [string]$Out = ""
)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
Set-Location $root

if ($Toolchain -eq "auto") {
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $Toolchain = if (Test-Path $vswhere) { "msvc" } else { "gnu" }
}
if ($Toolchain -eq "gnu") {
  # Workarounds for stable-x86_64-pc-windows-gnu without MSVC / MinGW (see README "Windows without MSVC"):
  #  1. rustc needs a `dlltool` for raw-dylib crates (windows-link); the bundled GNU dlltool needs `as`, which isn't shipped,
  #     so we use llvm-dlltool (+ llvm-windres for tauri-build resources) from an llvm-mingw release.
  #  2. GNU ld mis-links llvm-dlltool import libs (orphan IAT slots -> 0xc0000005), so we link with rust-lld,
  #     exposed to gcc as `ld.exe` via `-B`. The ld.exe is a tiny shim (win-gnu-ld-shim/main.rs) that re-quotes gcc's
  #     @response files for lld, otherwise paths with spaces break (`lld: error: unknown argument: -\`).
  $tools = Join-Path $root ".tools"
  $llvm = Join-Path $tools "llvm-mingw"
  if (-not (Test-Path (Join-Path $llvm "dlltool.exe"))) {
    $ver = "20261104"
    $zip = Join-Path $env:TEMP "llvm-mingw-$ver.zip"
    Write-Host "fetching llvm-mingw $ver (dlltool/windres only)..."
    Invoke-WebRequest "https://github.com/mstorsjo/llvm-mingw/releases/download/$ver/llvm-mingw-$ver-ucrt-x86_64.zip" -OutFile $zip
    New-Item -ItemType Directory -Force $llvm | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $z = [IO.Compression.ZipFile]::OpenRead($zip)
    foreach ($n in "llvm-dlltool.exe", "llvm-windres.exe", "llvm-rc.exe", "libLLVM-23.dll", "libc++.dll", "libunwind.dll", "libwinpthread-1.dll") {
      $e = $z.Entries | Where-Object { $_.FullName -like "*/bin/$n" -and $_.FullName -notlike "*-w64-mingw32/*" } | Select-Object -First 1
      [IO.Compression.ZipFileExtensions]::ExtractToFile($e, (Join-Path $llvm $n), $true)
    }
    $z.Dispose()
    Copy-Item (Join-Path $llvm "llvm-dlltool.exe") (Join-Path $llvm "dlltool.exe")
    & rustc +stable-x86_64-pc-windows-gnu -O -o (Join-Path $llvm "windres.exe") (Join-Path $root "win-gnu-ld-shim\windres.rs")
  }
  $lldDir = Join-Path $tools "lld-as-ld"
  if (-not (Test-Path (Join-Path $lldDir "ld.exe"))) {
    $sysroot = (& rustc +stable-x86_64-pc-windows-gnu --print sysroot).Trim()
    $bin = Join-Path $sysroot "lib\rustlib\x86_64-pc-windows-gnu\bin"
    New-Item -ItemType Directory -Force $lldDir | Out-Null
    Copy-Item (Join-Path $bin "rust-lld.exe") (Join-Path $lldDir "rust-lld.exe")
    Copy-Item (Join-Path $bin "*.dll") $lldDir
    & rustc +stable-x86_64-pc-windows-gnu -O -o (Join-Path $lldDir "ld.exe") (Join-Path $root "win-gnu-ld-shim\main.rs")
    if ($LASTEXITCODE) { throw "ld shim build failed" }
  }
  $env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"
  $env:PATH = "$llvm;$env:PATH"
  $env:CARGO_ENCODED_RUSTFLAGS = "-Clink-arg=-B" + ($lldDir -replace '\\', '/') + "/"
}
Write-Host "toolchain: $Toolchain"

if (-not $SkipBuild) {
  cargo build --release -p fake-daemon
  if ($LASTEXITCODE) { throw "daemon build failed" }
  Push-Location app
  pnpm install
  if ($Bundle) { pnpm tauri build --bundles nsis } else { pnpm tauri build --no-bundle }
  $code = $LASTEXITCODE
  Pop-Location
  if ($code) { throw "tauri build failed" }
}

Get-Process fake-daemon -ErrorAction SilentlyContinue | Stop-Process -Force   # cold start: the app spawns the daemon
Start-Sleep -Milliseconds 500
if (-not $Out) { $Out = Join-Path $root "bench-results-windows-c$Coalesce.json" }
$env:GENIE_BENCH = "1"
$env:GENIE_BENCH_OUT = $Out
$env:GENIE_COALESCE_MS = "$Coalesce"
Write-Host "running bench (a window opens for ~2 minutes; do not cover or minimise it)..."
$p = Start-Process -FilePath (Join-Path $root "target\release\genie-proto-37.exe") -ArgumentList "--bench" -PassThru
$p.WaitForExit()   # not -Wait: PS 5.1 would also wait for the detached daemon
Remove-Item Env:GENIE_BENCH, Env:GENIE_BENCH_OUT, Env:GENIE_COALESCE_MS
node summarize.mjs $Out
"--- sizes"
Get-Item target\release\genie-proto-37.exe, target\release\fake-daemon.exe | ForEach-Object { "{0,-22} {1,8:N2} MB" -f $_.Name, ($_.Length / 1MB) }
"{0,-22} {1,8:N2} MB" -f "app/dist", ((Get-ChildItem app\dist -Recurse | Measure-Object Length -Sum).Sum / 1MB)
Get-ChildItem target\release\bundle -Recurse -Include *.exe, *.msi -ErrorAction SilentlyContinue | ForEach-Object { "{0,-22} {1,8:N2} MB" -f $_.Name, ($_.Length / 1MB) }
"results: $Out"
