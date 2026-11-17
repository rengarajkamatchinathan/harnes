//! `genie-desktop`: the Tauri app (see `lib.rs`).
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

#[cfg(webview)]
fn main() -> std::process::ExitCode {
    match genie_desktop::shell::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("genie-desktop: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(webview))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "genie-desktop was built without the webview. On Linux, install the WebKitGTK 4.1 \
         development packages and build with `--features linux-webview`."
    );
    std::process::ExitCode::FAILURE
}
