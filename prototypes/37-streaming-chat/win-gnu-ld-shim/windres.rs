//! `windres.exe` shim (prototype 37 workaround): runs the sibling llvm-windres with --no-preprocess,
//! since tauri-build's generated resource.rc has no #include/#define and clang isn't available.
//! Build: rustc +stable-x86_64-pc-windows-gnu -O -o .tools/llvm-mingw/windres.exe win-gnu-ld-shim/windres.rs
use std::process::{exit, Command};
fn main() {
    let real = std::env::current_exe().unwrap().with_file_name("llvm-windres.exe");
    let status = Command::new(real)
        .arg("--no-preprocess")
        .args(std::env::args_os().skip(1))
        .status()
        .expect("failed to run llvm-windres.exe");
    exit(status.code().unwrap_or(1));
}
