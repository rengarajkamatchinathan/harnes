//! Emits `cfg(webview)` when the Tauri shell is compiled: always on Windows and macOS,
//! on Linux only with `--features linux-webview` (WebKitGTK dev packages needed).

fn main() {
    println!("cargo::rustc-check-cfg=cfg(webview)");
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    let opted_in = std::env::var_os("CARGO_FEATURE_LINUX_WEBVIEW").is_some();
    if !linux || opted_in {
        println!("cargo::rustc-cfg=webview");
        #[cfg(any(not(target_os = "linux"), feature = "linux-webview"))]
        tauri_build::build();
    }
}
