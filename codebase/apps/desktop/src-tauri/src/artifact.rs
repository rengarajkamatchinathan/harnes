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
//! React artifacts need `packages/artifact-runtime` (esbuild-wasm + bundled libraries),
//! which is not built yet: they get a placeholder page.

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
            }
        }
        // Shown through `<img>`: scripts in an SVG never run in image context.
        "svg" => Rendered {
            mime: "image/svg+xml",
            body: content.to_string(),
            scripts: vec![],
        },
        "react" => Rendered {
            mime: "text/html; charset=utf-8",
            body: inject_bridge(&format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head>\
                 <body style=\"font:14px system-ui;padding:16px;color:#555\">\
                 <p>React previews need the artifact runtime, which is coming soon. \
                 Use the Code tab to see the source.</p></body></html>",
                escape(title)
            )),
            scripts: vec![],
        },
        _ => Rendered {
            mime: "text/plain; charset=utf-8",
            body: content.to_string(),
            scripts: vec![],
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
    fn react_is_a_placeholder_and_svg_is_an_image() {
        assert!(
            render("react", "<x>", "export default 1")
                .body
                .contains("&lt;x&gt;")
        );
        assert_eq!(render("svg", "", "<svg/>").mime, "image/svg+xml");
        assert_eq!(
            render("document", "", "# hi").mime,
            "text/plain; charset=utf-8"
        );
    }
}
