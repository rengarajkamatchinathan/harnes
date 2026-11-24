//! The Browser tab as a browser (t47 follow-up): the page is laid out at the panel's size
//! and pixel density (`browser.viewport`, zoom folded in by the client), find in page
//! (`browser.find`) and what is under the pointer (`browser.hit`: cursor, link, whether
//! the page showed its own right-click menu).
//!
//! The viewport is an `Emulation.setDeviceMetricsOverride` on each tab as it is shown
//! ([`Browser::sync_casts`]). A headed (popped-out) browser keeps its real window size.

use genie_protocol::api::browser::{FindParams, FindResult, HitParams, HitResult, ViewportParams};
use serde_json::json;

use crate::backend::BrowserError;
use crate::cdp::CdpClient;
use crate::session::Browser;

/// Smallest and largest CSS viewport the panel can ask for.
const MIN_CSS: u32 = 200;
const MAX_CSS_W: u32 = 2560;
const MAX_CSS_H: u32 = 1600;
const MAX_SCALE: f64 = 4.0;
/// Longest find query.
const MAX_QUERY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub w: u32,
    pub h: u32,
    pub scale: f64,
}

impl Viewport {
    /// Clamp what a client asked for; `None` for a collapsed or hidden panel.
    pub fn from_params(p: &ViewportParams) -> Option<Viewport> {
        if p.width < 50 || p.height < 50 || !p.device_scale.is_finite() {
            return None;
        }
        Some(Viewport {
            w: p.width.clamp(MIN_CSS, MAX_CSS_W),
            h: p.height.clamp(MIN_CSS, MAX_CSS_H),
            scale: p.device_scale.clamp(0.25, MAX_SCALE),
        })
    }
}

const FIND_JS: &str = r#"(q, back, restart) => {
  if (restart) { const s = getSelection(); if (s) s.removeAllRanges(); }
  const text = ((document.body && document.body.innerText) || '').toLowerCase();
  const needle = q.toLowerCase();
  let n = 0;
  if (needle) for (let i = text.indexOf(needle); i !== -1 && n < 9999; i = text.indexOf(needle, i + needle.length)) n++;
  const found = n > 0 && window.find(q, false, back, true, false, false, false);
  return { matches: n, found: !!found };
}"#;

/// The first call installs a capture listener that remembers the last `contextmenu`
/// event; reading `defaultPrevented` afterwards tells whether the page showed its own menu.
const HIT_JS: &str = r#"(x, y) => {
  if (!window.__genieCtx) {
    const c = window.__genieCtx = { e: null, t: 0 };
    addEventListener('contextmenu', (e) => { c.e = e; c.t = Date.now(); }, true);
  }
  const c = window.__genieCtx;
  const pageMenu = !!(c.e && Date.now() - c.t < 1500 && c.e.defaultPrevented);
  let el = document.elementFromPoint(x, y);
  while (el && el.shadowRoot) {
    const inner = el.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === el) break;
    el = inner;
  }
  if (!el) return { cursor: 'default', link: null, editable: false, pageMenu };
  const a = el.closest && el.closest('a[href]');
  const plain = ['button', 'submit', 'checkbox', 'radio', 'range', 'color', 'file', 'image', 'reset'];
  const field = el.matches('input, textarea') && !el.disabled && !plain.includes(el.type);
  const editable = !!(el.isContentEditable || field);
  let cursor = getComputedStyle(el).cursor || 'auto';
  if (cursor === 'auto') {
    cursor = a ? 'pointer' : editable ? 'text' : 'default';
    if (cursor === 'default' && document.caretRangeFromPoint) {
      const r = document.caretRangeFromPoint(x, y);
      const n = r && r.startContainer;
      if (n && n.nodeType === 3) {
        const one = document.createRange();
        for (const k of [r.startOffset - 1, r.startOffset]) {
          if (k < 0 || k >= n.length || !n.data[k].trim()) continue;
          one.setStart(n, k); one.setEnd(n, k + 1);
          const b = one.getBoundingClientRect();
          if (x >= b.left && x <= b.right && y >= b.top && y <= b.bottom) { cursor = 'text'; break; }
        }
      }
    }
  }
  return { cursor, link: a ? a.href : null, editable, pageMenu };
}"#;

/// `Emulation.setDeviceMetricsOverride` for one tab.
pub(crate) async fn apply(
    c: &CdpClient,
    cdp_session: &str,
    v: Viewport,
) -> Result<(), BrowserError> {
    c.call_in(
        Some(cdp_session),
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": v.w, "height": v.h, "deviceScaleFactor": v.scale, "mobile": false,
            "screenWidth": v.w, "screenHeight": v.h,
        }),
    )
    .await?;
    Ok(())
}

impl Browser {
    /// `browser.viewport`: remember the panel's size and lay the session's tabs out at it.
    /// It never launches the browser.
    pub async fn set_viewport(&self, p: &ViewportParams) -> Result<(), BrowserError> {
        let Some(v) = Viewport::from_params(p) else {
            return Ok(());
        };
        let s = self.session(&p.session_id);
        if s.viewport() == Some(v) {
            return Ok(());
        }
        s.set_viewport(v);
        if self.backend.headed() {
            return Ok(());
        }
        let Some(c) = s.client().filter(|c| !c.is_closed()) else {
            return Ok(());
        };
        // The shown tab first; the others are laid out again when they are shown.
        if let Some(t) = s.active_tab() {
            apply(&c, &t.cdp_session, v).await?;
        }
        Ok(())
    }

    /// The viewport for a tab about to be shown (`None`: leave Chrome's own size).
    pub(crate) fn viewport_for(&self, session_id: &str) -> Option<Viewport> {
        if self.backend.headed() {
            return None;
        }
        self.existing_session(session_id)?.viewport()
    }

    /// `browser.find`. Like other input it pauses the agent during a turn.
    pub async fn find(&self, p: &FindParams) -> Result<FindResult, BrowserError> {
        let s = self.session(&p.session_id);
        let (Some(t), Some(c)) = (s.tab(&p.tab_id), s.client()) else {
            return Err(BrowserError::Invalid(format!("no tab {}", p.tab_id)));
        };
        let q: String = p.query.chars().take(MAX_QUERY).collect();
        if q.is_empty() {
            return Ok(FindResult {
                matches: 0,
                found: false,
            });
        }
        self.user_acted(&s);
        let q = serde_json::to_string(&q).map_err(|e| BrowserError::Invalid(e.to_string()))?;
        let expr = format!("({FIND_JS})({q}, {}, {})", p.backwards, p.restart);
        let r = c
            .call_in(
                Some(&t.cdp_session),
                "Runtime.evaluate",
                json!({"expression": expr, "returnByValue": true}),
            )
            .await?;
        let v = &r["result"]["value"];
        Ok(FindResult {
            matches: v["matches"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            found: v["found"].as_bool().unwrap_or(false),
        })
    }

    /// `browser.hit`: read-only, whoever is in control.
    pub async fn hit(&self, p: &HitParams) -> Result<HitResult, BrowserError> {
        let s = self.session(&p.session_id);
        let (Some(t), Some(c)) = (s.tab(&p.tab_id), s.client()) else {
            return Err(BrowserError::Invalid(format!("no tab {}", p.tab_id)));
        };
        let expr = format!("({HIT_JS})({}, {})", p.x, p.y);
        let r = c
            .call_in(
                Some(&t.cdp_session),
                "Runtime.evaluate",
                json!({"expression": expr, "returnByValue": true}),
            )
            .await?;
        Ok(hit_result(&r["result"]["value"]))
    }
}

fn hit_result(v: &serde_json::Value) -> HitResult {
    HitResult {
        cursor: v["cursor"].as_str().unwrap_or("default").to_string(),
        link: v["link"]
            .as_str()
            .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
            .map(String::from),
        editable: v["editable"].as_bool().unwrap_or(false),
        page_menu: v["pageMenu"].as_bool().unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(w: u32, h: u32, s: f64) -> ViewportParams {
        ViewportParams {
            session_id: "s".into(),
            width: w,
            height: h,
            device_scale: s,
        }
    }

    #[test]
    fn viewport_clamps() {
        assert_eq!(Viewport::from_params(&params(10, 600, 1.0)), None);
        assert_eq!(Viewport::from_params(&params(800, 600, f64::NAN)), None);
        assert_eq!(
            Viewport::from_params(&params(9000, 120, 9.0)),
            Some(Viewport {
                w: MAX_CSS_W,
                h: MIN_CSS,
                scale: MAX_SCALE
            })
        );
        assert_eq!(
            Viewport::from_params(&params(853, 671, 1.5)),
            Some(Viewport {
                w: 853,
                h: 671,
                scale: 1.5
            })
        );
    }

    #[test]
    fn hit_keeps_only_web_links() {
        let r = hit_result(
            &json!({"cursor": "pointer", "link": "javascript:alert(1)", "editable": false, "pageMenu": true}),
        );
        assert_eq!(r.link, None);
        assert!(r.page_menu);
        let r = hit_result(&json!({"cursor": "pointer", "link": "https://a.example/x"}));
        assert_eq!(r.link.as_deref(), Some("https://a.example/x"));
        assert_eq!(hit_result(&json!(null)).cursor, "default");
    }
}
