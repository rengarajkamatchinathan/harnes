//! The artifact scheme (05 §8, D28): `genie-artifact://localhost/<artifact>/<n>/…`
//! (Windows: `http://genie-artifact.localhost/…`), served by the Rust side from
//! `artifact.get`, never from the app origin.
//!
//! The page is shown in `<iframe sandbox="allow-scripts">` (opaque origin). Every
//! response carries the 05 §8 CSP. Because that CSP has no `'unsafe-inline'` for
//! scripts, inline `<script>` blocks of an HTML artifact are served as separate files
//! from the scheme (`inline-<k>.js`), and a small bridge script (`/_genie/bridge.js`)
//! implements the fixed postMessage schema and shims `alert/confirm/prompt`.
//!
//! React artifacts get a page that loads `packages/artifact-runtime` from
//! `/_genie/runtime/` on the same scheme (esbuild-wasm compiles the component inside the
//! sandbox; imports resolve only to the bundled libraries). The artifact source is served as
//! `source.js` (`globalThis.__GENIE_SOURCE__ = "<JSON string>"`), never inlined into HTML.

use std::path::{Path, PathBuf};

/// 05 §8, plus the Windows `http://genie-artifact.localhost` origin form.
pub const CSP: &str = "default-src 'none'; \
script-src genie-artifact: http://genie-artifact.localhost 'unsafe-eval' 'wasm-unsafe-eval'; \
style-src genie-artifact: http://genie-artifact.localhost 'unsafe-inline'; \
img-src genie-artifact: http://genie-artifact.localhost data: blob:; \
font-src genie-artifact: http://genie-artifact.localhost data:; \
connect-src 'none'; frame-src 'none'; form-action 'none'";

/// Injected first in every HTML artifact. Fixed schema: `ready`, `resize {height}`,
/// `runtime_error {message, stack}`, `request_fullscreen` (05 §8).
pub const BRIDGE_JS: &str = r#"(function () {
  var post = function (m) { try { parent.postMessage(m, "*"); } catch (e) {} };
  window.alert = function () {};
  window.confirm = function () { return false; };
  window.prompt = function () { return null; };
  window.open = function () { return null; };
  window.addEventListener("error", function (e) {
    post({ type: "runtime_error", message: String(e.message || e), stack: e.error && e.error.stack ? String(e.error.stack) : "" });
  });
  window.addEventListener("unhandledrejection", function (e) {
    var r = e.reason || {};
    post({ type: "runtime_error", message: String(r.message || r), stack: r.stack ? String(r.stack) : "" });
  });
  var last = 0;
  var report = function () {
    var h = Math.ceil(document.documentElement.scrollHeight);
    if (h !== last) { last = h; post({ type: "resize", height: h }); }
  };
  window.addEventListener("load", function () {
    post({ type: "ready" });
    report();
    if (window.ResizeObserver) new ResizeObserver(report).observe(document.documentElement);
  });
})();
"#;

/// `/<artifact_id>/<n>/<file>` → `(artifact_id, n, file)`; `file` defaults to `index.html`.
pub fn parse_path(path: &str) -> Option<(String, u32, String)> {
    let mut it = path.trim_start_matches('/').splitn(3, '/');
    let id = it.next().filter(|s| !s.is_empty())?;
    let n = it.next()?.parse().ok()?;
    let file = match it.next() {
        None | Some("") => "index.html".to_string(),
        Some(f) => f.to_string(),
    };
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((id.to_string(), n, file))
}

/// A rendered artifact: the page and the inline scripts pulled out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub mime: &'static str,
    pub body: String,
    pub scripts: Vec<String>,
    /// React artifacts: the script served as `source.js`.
    pub source_js: Option<String>,
}

/// Base styles of the React page (inline styles are allowed by the CSP; scripts are not).
const REACT_STYLE: &str = "html,body{margin:0}body{font:14px/1.5 system-ui,sans-serif;\
padding:16px;color:#1f2328;background:#fff}\
.genie-error{border:1px solid #e5534b;border-radius:8px;padding:12px;color:#a40e26;background:#fff5f5}\
.genie-error pre{white-space:pre-wrap;font:12px ui-monospace,monospace;margin:8px 0 0}";

/// The page for a React artifact (05 §8): bridge, runtime, source; no inline script.
pub fn react_page(title: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{}</title><script src=\"/_genie/bridge.js\"></script><style>{REACT_STYLE}</style></head>\
         <body><div id=\"root\"></div><script src=\"source.js\"></script>\
         <script src=\"/_genie/runtime/runtime.js\"></script></body></html>",
        escape(title)
    )
}

/// `source.js` for a React artifact: the code as a JSON string literal (valid JS).
pub fn react_source_js(content: &str) -> String {
    let lit = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".into());
    format!("globalThis.__GENIE_SOURCE__ = {lit};\n")
}

/// `/_genie/runtime/<rel>` → `rel` when it is a plain relative path inside the runtime.
pub fn runtime_rel(path: &str) -> Option<&str> {
    let rel = path.strip_prefix("/_genie/runtime/")?;
    let ok = !rel.is_empty()
        && rel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
        && rel
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'));
    ok.then_some(rel)
}

/// Content type by extension for runtime files.
pub fn runtime_mime(rel: &str) -> &'static str {
    match rel.rsplit_once('.').map(|(_, e)| e) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// Folders that may hold the built runtime (`packages/artifact-runtime/dist`), in order:
/// `GENIE_ARTIFACT_RUNTIME`, `<resources>/artifact-runtime`, and in debug builds the
/// workspace copies. Release builds also find it embedded with the frontend
/// (`dist/artifact-runtime`, copied there by the desktop Vite build).
pub fn runtime_dirs(resource_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Some(d) = std::env::var_os("GENIE_ARTIFACT_RUNTIME") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(r) = resource_dir {
        dirs.push(r.join("artifact-runtime"));
    }
    if cfg!(debug_assertions) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        dirs.push(manifest.join("../../../packages/artifact-runtime/dist"));
        dirs.push(manifest.join("../dist/artifact-runtime"));
    }
    dirs
}

/// Read `rel` (already checked by [`runtime_rel`]) from the first folder that has it.
pub fn read_runtime_file(dirs: &[PathBuf], rel: &str) -> Option<Vec<u8>> {
    dirs.iter().find_map(|d| std::fs::read(d.join(rel)).ok())
}

/// Build what the scheme serves for an artifact of `kind` with `content`.
pub fn render(kind: &str, title: &str, content: &str) -> Rendered {
    match kind {
        "html" => {
            let (html, scripts) = extract_inline_scripts(content);
            Rendered {
                mime: "text/html; charset=utf-8",
                body: inject_bridge(&html),
                scripts,
                source_js: None,
            }
        }
        // Shown through `<img>`: scripts in an SVG never run in image context.
        "svg" => Rendered {
            mime: "image/svg+xml",
            body: content.to_string(),
            scripts: vec![],
            source_js: None,
        },
        "react" => Rendered {
            mime: "text/html; charset=utf-8",
            body: react_page(title),
            scripts: vec![],
            source_js: Some(react_source_js(content)),
        },
        _ => Rendered {
            mime: "text/plain; charset=utf-8",
            body: content.to_string(),
            scripts: vec![],
            source_js: None,
        },
    }
}

fn inject_bridge(html: &str) -> String {
    let tag = "<script src=\"/_genie/bridge.js\"></script>";
    let lower = html.to_ascii_lowercase();
    if let Some(i) = lower.find("<head>") {
        let at = i + "<head>".len();
        format!("{}{}{}", &html[..at], tag, &html[at..])
    } else {
        format!("{tag}{html}")
    }
}

/// Replace every `<script>` without `src` by `<script src="inline-<k>.js">`.
pub fn extract_inline_scripts(html: &str) -> (String, Vec<String>) {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut scripts = vec![];
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<script") {
        let start = pos + rel;
        let Some(open_end) = lower[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let Some(close) = lower[open_end..].find("</script").map(|i| open_end + i) else {
            break;
        };
        let close_end = lower[close..]
            .find('>')
            .map_or(html.len(), |i| close + i + 1);
        let open_tag = &lower[start..open_end];
        out.push_str(&html[pos..start]);
        if open_tag.contains("src=") {
            out.push_str(&html[start..close_end]);
        } else {
            let attrs = if open_tag.contains("type=\"module\"") || open_tag.contains("type=module")
            {
                " type=\"module\""
            } else {
                ""
            };
            out.push_str(&format!(
                "<script{attrs} src=\"inline-{}.js\"></script>",
                scripts.len()
            ));
            scripts.push(html[open_end..close].to_string());
        }
        pos = close_end;
    }
    out.push_str(&html[pos..]);
    (out, scripts)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(
            parse_path("/art_1/2/"),
            Some(("art_1".into(), 2, "index.html".into()))
        );
        assert_eq!(
            parse_path("/art_1/2/inline-0.js"),
            Some(("art_1".into(), 2, "inline-0.js".into()))
        );
        assert_eq!(parse_path("/art_1/x/"), None);
        assert_eq!(parse_path("/../2/"), None);
        assert_eq!(parse_path("/"), None);
    }

    #[test]
    fn inline_scripts_move_to_files() {
        let html = "<html><head><title>t</title></head><body><SCRIPT>alert(1)</SCRIPT>\
                    <script src=\"x.js\"></script><script type=\"module\">let a=1</script></body></html>";
        let r = render("html", "t", html);
        assert_eq!(
            r.scripts,
            vec!["alert(1)".to_string(), "let a=1".to_string()]
        );
        assert!(r.body.contains("<script src=\"inline-0.js\"></script>"));
        assert!(
            r.body
                .contains("<script type=\"module\" src=\"inline-1.js\"></script>")
        );
        assert!(r.body.contains("<script src=\"x.js\"></script>"));
        assert!(
            r.body
                .contains("<head><script src=\"/_genie/bridge.js\"></script><title>")
        );
        assert!(!r.body.contains("alert(1)"));
    }

    #[test]
    fn csp_matches_spec() {
        for part in [
            "default-src 'none'",
            "connect-src 'none'",
            "frame-src 'none'",
            "form-action 'none'",
        ] {
            assert!(CSP.contains(part), "{part}");
        }
        assert!(!CSP.contains("script-src genie-artifact: 'unsafe-inline'"));
    }

    #[test]
    fn react_page_loads_the_runtime_and_never_inlines_the_source() {
        let src = "export default () => <p>{\"</script><script>alert(1)</script>\"}</p>";
        let r = render("react", "<x>", src);
        assert!(r.body.contains("<title>&lt;x&gt;</title>"));
        assert!(
            r.body
                .contains("<script src=\"/_genie/bridge.js\"></script>")
        );
        assert!(r.body.contains("<script src=\"source.js\"></script>"));
        assert!(
            r.body
                .contains("<script src=\"/_genie/runtime/runtime.js\"></script>")
        );
        assert!(!r.body.contains("alert(1)"));
        // Every <script> in the page has a src (the CSP has no 'unsafe-inline' for scripts).
        assert_eq!(
            r.body.matches("<script").count(),
            r.body.matches("<script src=").count()
        );
        let js = r.source_js.unwrap();
        let lit = js
            .strip_prefix("globalThis.__GENIE_SOURCE__ = ")
            .unwrap()
            .trim_end()
            .trim_end_matches(';');
        assert_eq!(serde_json::from_str::<String>(lit).unwrap(), src);
    }

    #[test]
    fn runtime_paths_stay_inside_the_runtime() {
        assert_eq!(
            runtime_rel("/_genie/runtime/runtime.js"),
            Some("runtime.js")
        );
        assert_eq!(
            runtime_rel("/_genie/runtime/libs/fonts/KaTeX_Main-Regular.woff2"),
            Some("libs/fonts/KaTeX_Main-Regular.woff2")
        );
        for bad in [
            "/_genie/runtime/../secret",
            "/_genie/runtime/libs/../../x",
            "/_genie/runtime/",
            "/_genie/runtime//abs",
            "/_genie/runtime/a%2e%2e/b",
            "/_genie/runtime/.hidden",
            "/other/runtime.js",
        ] {
            assert_eq!(runtime_rel(bad), None, "{bad}");
        }
        assert_eq!(runtime_mime("libs/katex.css"), "text/css; charset=utf-8");
        assert_eq!(runtime_mime("x.woff2"), "font/woff2");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("libs")).unwrap();
        std::fs::write(dir.path().join("libs/react.js"), "x").unwrap();
        let dirs = vec![dir.path().join("missing"), dir.path().to_path_buf()];
        assert_eq!(
            read_runtime_file(&dirs, "libs/react.js"),
            Some(b"x".to_vec())
        );
        assert_eq!(read_runtime_file(&dirs, "libs/none.js"), None);
    }

    #[test]
    fn svg_is_an_image() {
        assert_eq!(render("svg", "", "<svg/>").mime, "image/svg+xml");
        assert_eq!(
            render("document", "", "# hi").mime,
            "text/plain; charset=utf-8"
        );
    }
}
