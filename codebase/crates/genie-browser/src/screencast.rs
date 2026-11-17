//! Screencast and take-over input (05 §9, D56, SA-23/SA-24), as library functions the
//! daemon calls from `browser.screencast.start/ack/stop` and `browser.input`.
//!
//! One CDP `Page.startScreencast` per tab, fanned out to every stream watching it.
//! Flow control is ack-based with **one frame in flight per client**: a frame goes to each
//! stream that has acked its previous one; the others get the newest frame when they ack
//! (older frames are dropped). Chrome's own `Page.screencastFrameAck` is sent as soon as
//! at least one stream can take a frame, otherwise held until a client acks, so Chrome
//! never runs ahead of the fastest client.

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
use crate::host::{Control, FrameSink};
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

/// All screencasts of the daemon.
#[derive(Default)]
pub struct Casts {
    /// Keyed by CDP session id (one per tab).
    tabs: Mutex<HashMap<String, TabCast>>,
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
    pub async fn screencast_start(
        &self,
        p: &ScreencastStartParams,
        sink: Arc<dyn FrameSink>,
    ) -> Result<StreamIdResult, BrowserError> {
        let s = self.session(&p.session_id);
        let tab = match &p.tab_id {
            Some(id) => s.tab(id),
            None => s.active_tab(),
        }
        .ok_or_else(|| BrowserError::Launch("no browser tab to show".into()))?;
        let client = self.client().await?;
        let stream_id = format!("sc{}", self.casts.next.fetch_add(1, Ordering::SeqCst) + 1);
        let stream = Stream {
            stream_id: stream_id.clone(),
            sink,
            in_flight: false,
            last_sent: 0,
        };
        let existing = self
            .casts
            .tabs
            .lock()
            .map(|mut g| match g.get_mut(&tab.cdp_session) {
                Some(c) if c.client.same_connection(&client) => {
                    c.streams.push(stream);
                    None
                }
                _ => Some(stream),
            })
            .map_err(|_| BrowserError::Launch("screencast state poisoned".into()))?;
        let Some(stream) = existing else {
            return Ok(StreamIdResult { stream_id });
        };
        let quality = if p.quality == 0 {
            DEFAULT_QUALITY
        } else {
            p.quality.min(100)
        };
        let max_w = p.max_width.clamp(1, MAX_CSS_WIDTH * MAX_DEVICE_SCALE);
        let max_h = p.max_height.clamp(1, MAX_CSS_HEIGHT * MAX_DEVICE_SCALE);
        let stop = CancellationToken::new();
        let rx = client.subscribe();
        if let Ok(mut g) = self.casts.tabs.lock() {
            g.insert(
                tab.cdp_session.clone(),
                TabCast {
                    session_id: p.session_id.clone(),
                    cdp_session: tab.cdp_session.clone(),
                    client: client.clone(),
                    streams: vec![stream],
                    latest: None,
                    latest_seq: 0,
                    held_ack: None,
                    stop: stop.clone(),
                },
            );
        }
        self.spawn_pump(rx, tab.clone(), stop);
        client
            .call_in(
                Some(&tab.cdp_session),
                "Page.startScreencast",
                json!({
                    "format": "jpeg", "quality": quality,
                    "maxWidth": max_w, "maxHeight": max_h, "everyNthFrame": 1,
                }),
            )
            .await?;
        Ok(StreamIdResult { stream_id })
    }

    fn spawn_pump(
        &self,
        mut rx: broadcast::Receiver<CdpEvent>,
        tab: TabHandle,
        stop: CancellationToken,
    ) {
        let me = self.weak();
        self.tasks.spawn(async move {
            loop {
                let e = tokio::select! {
                    () = stop.cancelled() => break,
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
            if let Some(b) = me.upgrade()
                && let Ok(mut g) = b.casts.tabs.lock()
            {
                g.remove(&tab.cdp_session);
            }
        });
    }

    /// `browser.screencast.ack`.
    pub fn screencast_ack(&self, p: &ScreencastAckParams) -> Result<(), BrowserError> {
        let ok = self
            .casts
            .tabs
            .lock()
            .map(|mut g| g.values_mut().any(|c| c.on_ack(&p.stream_id, p.frame_seq)))
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(BrowserError::Launch(format!("no stream {}", p.stream_id)))
        }
    }

    /// `browser.screencast.stop`. The CDP screencast stops with its last stream.
    pub async fn screencast_stop(&self, p: &StreamIdParams) {
        let ended = self.casts.tabs.lock().ok().and_then(|mut g| {
            let key = g
                .iter()
                .find(|(_, c)| c.streams.iter().any(|s| s.stream_id == p.stream_id))
                .map(|(k, _)| k.clone())?;
            let c = g.get_mut(&key)?;
            c.streams.retain(|s| s.stream_id != p.stream_id);
            if c.streams.is_empty() {
                let c = g.remove(&key)?;
                c.stop.cancel();
                Some((c.client, c.cdp_session))
            } else {
                c.release_ack();
                None
            }
        });
        if let Some((client, sid)) = ended {
            let _ = client
                .call_in(Some(&sid), "Page.stopScreencast", json!({}))
                .await;
        }
    }

    /// Stream ids currently watching a session's tabs.
    pub fn streams_of(&self, session_id: &str) -> Vec<String> {
        self.casts
            .tabs
            .lock()
            .map(|g| {
                g.values()
                    .filter(|c| c.session_id == session_id)
                    .flat_map(|c| c.streams.iter().map(|s| s.stream_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `browser.input` during take-over (05 §9, SA-24). Any input except plain pointer
    /// movement pauses the agent (tools return `user_in_control` until Hand back).
    pub async fn input(&self, p: &BrowserInputParams) -> Result<(), BrowserError> {
        let s = self.session(&p.session_id);
        let tab = s
            .tab(&p.tab_id)
            .ok_or_else(|| BrowserError::Launch(format!("no tab {}", p.tab_id)))?;
        if crate::input::takes_control(&p.event) && s.control() != Control::User {
            s.set_control(Control::User);
            self.host.control_changed(&p.session_id, Control::User);
        }
        let client = self.client().await?;
        let (method, params) = crate::input::to_cdp(&p.event);
        // Fire and forget keeps input-to-frame latency low; order is kept by the pipe.
        client.send(Some(&tab.cdp_session), method, params);
        Ok(())
    }
}

/// End every screencast of a session (Stop, session end).
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
