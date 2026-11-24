//! Screencast and take-over input (05 §9, D56, SA-23/SA-24), as library functions the
//! daemon calls from `browser.screencast.start/ack/stop` and `browser.input`.
//!
//! A client stream is a **watcher of a session**, not of a tab: it can start before the
//! agent has opened any tab, follows the session's active tab (new tab, tab switch, tab
//! closed, Chrome restarted or popped out), and is only pinned to one tab when the client
//! names it. [`Browser::sync_casts`] reconciles watchers with the session's tabs; the tab
//! code calls [`Browser::spawn_sync`] whenever the active tab may have changed.
//!
//! One CDP `Page.startScreencast` per tab, fanned out to every stream watching it.
//! Flow control is ack-based with **one frame in flight per client**: a frame goes to each
//! stream that has acked its previous one; the others get the newest frame when they ack
//! (older frames are dropped). Chrome's own `Page.screencastFrameAck` is sent as soon as
//! at least one stream can take a frame, otherwise held until a client acks, so Chrome
//! never runs ahead of the fastest client. A stream that joins a running cast gets the
//! newest frame at once (Chrome only sends frames when the page changes).
//!
//! Frame geometry: `w`/`h` are the viewport in **CSS pixels**; `device_scale` is JPEG
//! pixels per CSS pixel. Clients map pointer positions into `0..w` × `0..h`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use genie_protocol::api::browser::{
    BrowserInputParams, ScreencastAckParams, ScreencastStartParams, StreamIdParams, StreamIdResult,
};
use genie_protocol::event::BrowserFrame;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::backend::BrowserError;
use crate::cdp::{CdpClient, CdpEvent};
use crate::host::FrameSink;
use crate::session::{Browser, TabHandle};

/// SA-23 defaults.
pub const DEFAULT_QUALITY: u8 = 60;
pub const MAX_CSS_WIDTH: u32 = 1280;
pub const MAX_CSS_HEIGHT: u32 = 800;
pub const MAX_DEVICE_SCALE: u32 = 2;

struct Stream {
    stream_id: String,
    sink: Arc<dyn FrameSink>,
    in_flight: bool,
    last_sent: u64,
}

struct TabCast {
    /// Distinguishes a restarted cast of the same CDP session from the old one's pump.
    generation: u64,
    session_id: String,
    cdp_session: String,
    client: CdpClient,
    streams: Vec<Stream>,
    latest: Option<BrowserFrame>,
    latest_seq: u64,
    /// Chrome's frame number waiting for an ack.
    held_ack: Option<i64>,
    stop: CancellationToken,
}

/// A client's stream: which session it watches and where it is attached now.
struct Watcher {
    session_id: String,
    sink: Arc<dyn FrameSink>,
    /// Pinned to one tab (`tab_id` given at start); otherwise follows the active tab.
    pin: Option<String>,
    quality: u8,
    max_w: u32,
    max_h: u32,
    /// The CDP session (tab) this stream is attached to.
    attached: Option<String>,
}

/// All screencasts of the daemon. Lock order: `watchers` before `tabs`.
#[derive(Default)]
pub struct Casts {
    /// Keyed by CDP session id (one per tab).
    tabs: Mutex<HashMap<String, TabCast>>,
    /// Keyed by stream id.
    watchers: Mutex<HashMap<String, Watcher>>,
    /// Serialises [`Browser::sync_casts`].
    sync: tokio::sync::Mutex<()>,
    next: AtomicU64,
}

impl TabCast {
    fn send(&mut self, i: usize) {
        if let Some(f) = &self.latest {
            let s = &mut self.streams[i];
            let mut f = f.clone();
            f.stream_id.clone_from(&s.stream_id);
            s.sink.frame(f);
            s.in_flight = true;
            s.last_sent = self.latest_seq;
        }
    }

    fn release_ack(&mut self) {
        if let Some(n) = self.held_ack.take() {
            self.client.send(
                Some(&self.cdp_session),
                "Page.screencastFrameAck",
                json!({"sessionId": n}),
            );
        }
    }

    /// A frame from Chrome.
    fn on_frame(&mut self, frame: BrowserFrame, cdp_frame: i64) {
        self.latest_seq += 1;
        let mut f = frame;
        f.frame_seq = self.latest_seq;
        self.latest = Some(f);
        let mut any = false;
        for i in 0..self.streams.len() {
            if !self.streams[i].in_flight {
                self.send(i);
                any = true;
            }
        }
        self.held_ack = Some(cdp_frame);
        if any {
            self.release_ack();
        }
    }

    /// A client acked `frame_seq`.
    fn on_ack(&mut self, stream_id: &str, frame_seq: u64) -> bool {
        let Some(i) = self.streams.iter().position(|s| s.stream_id == stream_id) else {
            return false;
        };
        if frame_seq < self.streams[i].last_sent {
            return true; // stale ack
        }
        self.streams[i].in_flight = false;
        if self.latest_seq > self.streams[i].last_sent {
            self.send(i);
        }
        self.release_ack();
        true
    }

    /// Add a stream; it gets the newest frame at once.
    fn join(&mut self, stream_id: &str, sink: Arc<dyn FrameSink>) {
        self.streams.push(Stream {
            stream_id: stream_id.to_string(),
            sink,
            in_flight: false,
            last_sent: 0,
        });
        let i = self.streams.len() - 1;
        if self.latest.is_some() {
            self.send(i);
        }
    }
}

/// Remove `stream_id` from the cast at `key`; returns the cast when it has no streams left
/// (the caller stops Chrome's screencast).
fn detach(tabs: &mut HashMap<String, TabCast>, key: &str, stream_id: &str) -> Option<TabCast> {
    let c = tabs.get_mut(key)?;
    c.streams.retain(|s| s.stream_id != stream_id);
    if c.streams.is_empty() {
        let c = tabs.remove(key)?;
        c.stop.cancel();
        Some(c)
    } else {
        c.release_ack();
        None
    }
}

/// Image width/height from a JPEG's SOF marker.
pub fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return Some((u32::from(w), u32::from(h)));
        }
        i += 2 + len;
    }
    None
}

fn frame_from(e: &CdpEvent, tab_id: &str) -> Option<(BrowserFrame, i64)> {
    let p = &e.params;
    let data = p["data"].as_str()?.to_string();
    let n = p["sessionId"].as_i64()?;
    let m = &p["metadata"];
    let css_w = m["deviceWidth"].as_f64().unwrap_or(0.0);
    let css_h = m["deviceHeight"].as_f64().unwrap_or(0.0);
    let scale = crate::session::decode_b64(&data)
        .and_then(|b| jpeg_size(&b))
        .filter(|_| css_w > 0.0)
        .map(|(w, _)| f64::from(w) / css_w)
        .unwrap_or(1.0);
    let ts = m["timestamp"]
        .as_f64()
        .map(crate::time::rfc3339_from_unix)
        .unwrap_or_default();
    Some((
        BrowserFrame {
            stream_id: String::new(),
            frame_seq: 0,
            tab_id: tab_id.to_string(),
            jpeg_b64: data,
            w: css_w.round() as u32,
            h: css_h.round() as u32,
            device_scale: scale,
            ts,
        },
        n,
    ))
}

impl Browser {
    /// `browser.screencast.start` (SA-23: JPEG, ≤ 1280×800 CSS px × device scale ≤ 2).
    ///
    /// Without `tab_id` the stream follows the session's active tab and succeeds even
    /// when the agent hasn't opened one yet (frames start when it does). It never launches
    /// the browser.
    pub async fn screencast_start(
        &self,
        p: &ScreencastStartParams,
        sink: Arc<dyn FrameSink>,
    ) -> Result<StreamIdResult, BrowserError> {
        if let Some(id) = &p.tab_id
            && self.session(&p.session_id).tab(id).is_none()
        {
            return Err(BrowserError::Launch(format!("no tab {id}")));
        }
        let stream_id = format!("sc{}", self.casts.next.fetch_add(1, Ordering::SeqCst) + 1);
        let quality = if p.quality == 0 {
            DEFAULT_QUALITY
        } else {
            p.quality.min(100)
        };
        let watcher = Watcher {
            session_id: p.session_id.clone(),
            sink,
            pin: p.tab_id.clone(),
            quality,
            max_w: p.max_width.clamp(1, MAX_CSS_WIDTH * MAX_DEVICE_SCALE),
            max_h: p.max_height.clamp(1, MAX_CSS_HEIGHT * MAX_DEVICE_SCALE),
            attached: None,
        };
        self.casts
            .watchers
            .lock()
            .map_err(|_| BrowserError::Launch("screencast state poisoned".into()))?
            .insert(stream_id.clone(), watcher);
        self.sync_casts(&p.session_id).await;
        Ok(StreamIdResult { stream_id })
    }

    /// Reconcile a session's streams with its tabs in the background (the active tab
    /// changed, a tab closed, the connection changed).
    pub(crate) fn spawn_sync(&self, session_id: &str) {
        let me = self.weak();
        let sid = session_id.to_string();
        self.tasks.spawn(async move {
            if let Some(b) = me.upgrade() {
                b.sync_casts(&sid).await;
            }
        });
    }

    /// Attach each of the session's streams to the tab it should show (its pinned tab, else
    /// the active one), starting and stopping CDP screencasts as needed.
    pub async fn sync_casts(&self, session_id: &str) {
        let _serial = self.casts.sync.lock().await;
        let s = self.existing_session(session_id);
        let client = s
            .as_ref()
            .and_then(|s| s.client())
            .filter(|c| !c.is_closed());
        let mut to_stop: Vec<(CdpClient, String)> = Vec::new();
        let mut to_start: Vec<(TabHandle, u8, u32, u32, CancellationToken)> = Vec::new();
        {
            let (Ok(mut ws), Ok(mut tabs)) = (self.casts.watchers.lock(), self.casts.tabs.lock())
            else {
                return;
            };
            for (id, w) in ws.iter_mut().filter(|(_, w)| w.session_id == session_id) {
                let desired = match (&s, &client) {
                    (Some(s), Some(_)) => match &w.pin {
                        Some(t) => s.tab(t),
                        None => s.active_tab(),
                    },
                    _ => None,
                };
                let live = w.attached.as_ref().filter(|k| {
                    tabs.get(k.as_str()).is_some_and(|c| {
                        c.streams.iter().any(|x| &x.stream_id == id)
                            && client
                                .as_ref()
                                .is_some_and(|cl| c.client.same_connection(cl))
                    })
                });
                if live == desired.as_ref().map(|t| &t.cdp_session) {
                    continue;
                }
                if let Some(k) = w.attached.take()
                    && let Some(c) = detach(&mut tabs, &k, id)
                {
                    to_stop.push((c.client, c.cdp_session));
                }
                let (Some(t), Some(cl)) = (desired, &client) else {
                    continue;
                };
                match tabs.get_mut(&t.cdp_session) {
                    Some(c) if c.client.same_connection(cl) => c.join(id, w.sink.clone()),
                    _ => {
                        if let Some(old) = tabs.remove(&t.cdp_session) {
                            old.stop.cancel();
                        }
                        let generation = self.casts.next.fetch_add(1, Ordering::SeqCst) + 1;
                        let stop = CancellationToken::new();
                        let mut cast = TabCast {
                            generation,
                            session_id: session_id.to_string(),
                            cdp_session: t.cdp_session.clone(),
                            client: cl.clone(),
                            streams: Vec::new(),
                            latest: None,
                            latest_seq: 0,
                            held_ack: None,
                            stop: stop.clone(),
                        };
                        cast.join(id, w.sink.clone());
                        tabs.insert(t.cdp_session.clone(), cast);
                        // Subscribe before Chrome can send the first frame.
                        self.spawn_pump(cl.subscribe(), t.clone(), generation, stop.clone());
                        to_start.push((t.clone(), w.quality, w.max_w, w.max_h, stop));
                    }
                }
                w.attached = Some(t.cdp_session.clone());
            }
        }
        for (c, sid) in to_stop {
            let _ = c
                .call_in(Some(&sid), "Page.stopScreencast", json!({}))
                .await;
        }
        let Some(client) = client else { return };
        let viewport = self.viewport_for(session_id);
        for (t, quality, max_w, max_h, stop) in to_start {
            // Lay the page out at the panel's size before its first frame.
            if let Some(v) = viewport
                && let Err(e) = crate::view::apply(&client, &t.cdp_session, v).await
            {
                tracing::warn!(error = %e, tab = %t.tab_id, "viewport override failed");
            }
            let r = client
                .call_in(
                    Some(&t.cdp_session),
                    "Page.startScreencast",
                    json!({
                        "format": "jpeg", "quality": quality,
                        "maxWidth": max_w, "maxHeight": max_h, "everyNthFrame": 1,
                    }),
                )
                .await;
            if let Err(e) = r {
                tracing::warn!(error = %e, tab = %t.tab_id, "startScreencast failed");
                stop.cancel();
            }
        }
    }

    fn spawn_pump(
        &self,
        mut rx: broadcast::Receiver<CdpEvent>,
        tab: TabHandle,
        generation: u64,
        stop: CancellationToken,
    ) {
        let me = self.weak();
        self.tasks.spawn(async move {
            let mut cancelled = false;
            loop {
                let e = tokio::select! {
                    () = stop.cancelled() => { cancelled = true; break }
                    e = rx.recv() => e,
                };
                match e {
                    Ok(e)
                        if e.method == "Page.screencastFrame"
                            && e.session_id.as_deref() == Some(tab.cdp_session.as_str()) =>
                    {
                        let Some(b) = me.upgrade() else { break };
                        if let Some((f, n)) = frame_from(&e, &tab.tab_id)
                            && let Ok(mut g) = b.casts.tabs.lock()
                            && let Some(c) = g.get_mut(&tab.cdp_session)
                            && c.generation == generation
                        {
                            c.on_frame(f, n);
                        }
                    }
                    Ok(e)
                        if e.method == "Target.detachedFromTarget"
                            && e.params["sessionId"].as_str() == Some(tab.cdp_session.as_str()) =>
                    {
                        break;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            if cancelled {
                return;
            }
            // The tab or the connection went away: drop the cast and re-attach the
            // session's streams to whatever tab is active now.
            let Some(b) = me.upgrade() else { return };
            let session = b.casts.tabs.lock().ok().and_then(|mut g| {
                if g.get(&tab.cdp_session)
                    .is_some_and(|c| c.generation == generation)
                {
                    g.remove(&tab.cdp_session).map(|c| c.session_id)
                } else {
                    None
                }
            });
            if let Some(sid) = session {
                b.spawn_sync(&sid);
            }
        });
    }

    /// `browser.screencast.ack`.
    pub fn screencast_ack(&self, p: &ScreencastAckParams) -> Result<(), BrowserError> {
        let acked = self
            .casts
            .tabs
            .lock()
            .map(|mut g| g.values_mut().any(|c| c.on_ack(&p.stream_id, p.frame_seq)))
            .unwrap_or(false);
        // A stream between tabs (none active yet, tab switching) has nothing to ack.
        let known = acked
            || self
                .casts
                .watchers
                .lock()
                .is_ok_and(|g| g.contains_key(&p.stream_id));
        if known {
            Ok(())
        } else {
            Err(BrowserError::Launch(format!("no stream {}", p.stream_id)))
        }
    }

    /// `browser.screencast.stop`. The CDP screencast stops with its last stream.
    pub async fn screencast_stop(&self, p: &StreamIdParams) {
        let ended = {
            let (Ok(mut ws), Ok(mut tabs)) = (self.casts.watchers.lock(), self.casts.tabs.lock())
            else {
                return;
            };
            ws.remove(&p.stream_id)
                .and_then(|w| w.attached)
                .and_then(|k| detach(&mut tabs, &k, &p.stream_id))
        };
        if let Some(c) = ended {
            let _ = c
                .client
                .call_in(Some(&c.cdp_session), "Page.stopScreencast", json!({}))
                .await;
        }
    }

    /// Stream ids currently watching a session.
    pub fn streams_of(&self, session_id: &str) -> Vec<String> {
        self.casts
            .watchers
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|(_, w)| w.session_id == session_id)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `browser.input` (05 §9, SA-24). During a turn, any input except plain pointer
    /// movement pauses the agent (tools return `user_in_control` until Hand back); while
    /// the agent is idle the user can use the page directly (t47).
    pub async fn input(&self, p: &BrowserInputParams) -> Result<(), BrowserError> {
        let s = self.session(&p.session_id);
        // The browser went away (popped-out window closed): re-open the tabs first.
        self.recover(&s).await?;
        let tab = s
            .tab(&p.tab_id)
            .ok_or_else(|| BrowserError::Launch(format!("no tab {}", p.tab_id)))?;
        // During a turn this pauses the agent; while it's idle the user just browses.
        if crate::input::takes_control(&p.event) {
            self.user_acted(&s);
        }
        // The connection the tab lives on (the daemon's current one may be newer).
        let client = match s.client().filter(|c| !c.is_closed()) {
            Some(c) => c,
            None => self.client().await?,
        };
        let (method, params) = crate::input::to_cdp(&p.event);
        // Fire and forget keeps input-to-frame latency low; order is kept by the pipe.
        client.send(Some(&tab.cdp_session), method, params);
        Ok(())
    }
}

/// End every screencast of a session (session end).
pub(crate) async fn stop_session(b: &Browser, session_id: &str) {
    for id in b.streams_of(session_id) {
        b.screencast_stop(&StreamIdParams { stream_id: id }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_dims() {
        // SOI, APP0 (len 16), SOF0 with height 800 width 1280.
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        b.extend_from_slice(&[0; 14]);
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x03, 0x20, 0x05, 0x00, 0x03]);
        b.extend_from_slice(&[0; 12]);
        assert_eq!(jpeg_size(&b), Some((1280, 800)));
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0x00]), None);
    }
}
