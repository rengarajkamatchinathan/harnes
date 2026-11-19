// Browser tab (05 §9, D56): screencast frames on a <canvas>, agent-control banner, tab
// strip, take over (input forwarded in CSS pixels), hand back, stop, pop out.
//
// The screencast stream watches the *session*: it is started once when the tab shows and
// the daemon attaches it to whichever tab the agent has active (including the first one,
// opened after the panel), so the panel never has to restart it.
import { useCallback, useEffect, useRef, useState } from "react";
import type { BrowserControl, BrowserStatus, InputEvent, TabInfo } from "@genie/protocol";
import { Badge, Banner, Button, EmptyState } from "@genie/ui";
import { t } from "../../i18n";
import { isBusy } from "../../state/sessionModel";
import { useApp, useSession } from "../context";

/** Canvas point (client px) → page point in CSS px of the viewport (`0..w` × `0..h`). */
export function mapPoint(clientX: number, clientY: number, rect: { left: number; top: number; width: number; height: number }, frame: { w: number; h: number }) {
  const clamp = (v: number, max: number) => Math.min(max, Math.max(0, v));
  return {
    x: clamp(((clientX - rect.left) / rect.width) * frame.w, frame.w),
    y: clamp(((clientY - rect.top) / rect.height) * frame.h, frame.h),
  };
}

export function modifiers(e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): number {
  return (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
}

/** Text a key press types: printable keys without a command modifier (Enter's "\r" is added by the daemon). */
export function keyText(e: { key: string; ctrlKey: boolean; metaKey: boolean }): string | null {
  return e.key.length === 1 && !e.ctrlKey && !e.metaKey ? e.key : null;
}

/** Cmd/Ctrl+V: left to the native paste event, which carries Genie's clipboard text (SA-24). */
export function isPasteChord(e: { key: string; ctrlKey: boolean; metaKey: boolean }): boolean {
  return (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v";
}

/** Cmd/Ctrl+C or +X: the page's selection goes to Genie's clipboard (`browser.copy`, SA-24). */
export function copyChord(e: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }): "copy" | "cut" | null {
  if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return null;
  const k = e.key.toLowerCase();
  return k === "c" ? "copy" : k === "x" ? "cut" : null;
}

/** base64 JPEG → decoded bitmap (off the main thread where supported). */
async function decodeFrame(b64: string): Promise<CanvasImageSource & { width: number; height: number; close?: () => void }> {
  if (typeof createImageBitmap === "function") {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
  }
  const img = new Image();
  img.src = `data:image/jpeg;base64,${b64}`;
  await img.decode();
  return Object.assign(img, { width: img.naturalWidth, height: img.naturalHeight });
}

export type BannerState = "idle" | "agent" | "user" | "handoff" | "waiting";

/** The banner pill from the last `browser.control` event and whether a turn is running. */
export function bannerState(control: BrowserControl | null, busy: boolean): BannerState {
  switch (control?.control) {
    case "user":
      return "user";
    case "handoff":
      return "handoff";
    case "waiting_for_approval":
      return busy ? "waiting" : "idle";
    case "agent":
      return busy ? "agent" : "idle";
    default:
      return "idle";
  }
}

const MOUSE_BUTTONS = ["left", "middle", "right"];

/** Streams this window stopped: their frames (still in the session model) are not shown or acked. */
const deadStreams = new Set<string>();

export function BrowserPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const store = useSession(sessionId);
  const canvas = useRef<HTMLCanvasElement>(null);
  const keys = useRef<HTMLTextAreaElement>(null);
  const pendingMove = useRef<InputEvent | null>(null);
  const answeredChooser = useRef<string | null>(null);
  const [status, setStatus] = useState<BrowserStatus | null>(null);
  const [tabs, setTabs] = useState<TabInfo[]>([]);
  const [stream, setStream] = useState<string | null>(null);
  const [popping, setPopping] = useState(false);

  const model = store?.model;
  const raw = model?.browser.frame ?? null;
  const frame = raw && !deadStreams.has(raw.stream_id) ? raw : null;
  const control = model?.browser.control ?? null;
  const chooser = model?.browser.fileChooser ?? null;
  const actions = model?.browser.actions ?? [];
  const busy = model ? isBusy(model) : false;
  const state = bannerState(control, busy);
  const inControl = state === "user" || state === "handoff";
  const hasTab = tabs.length > 0 || !!frame;
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });

  useEffect(() => {
    app.client.browser.status({}).then(setStatus).catch(() => setStatus(null));
  }, [app]);

  // One stream per visible tab (SA-23: JPEG 60, ≤ 1280×800); it follows the agent's active tab.
  useEffect(() => {
    let id: string | null = null;
    let live = true;
    app.client.browser.screencast
      .start({ session_id: sessionId, quality: 60, max_width: 1280, max_height: 800 })
      .then((r) => {
        id = r.stream_id;
        if (live) setStream(r.stream_id);
        else {
          deadStreams.add(r.stream_id);
          void app.client.browser.screencast.stop({ stream_id: r.stream_id }).catch(() => {});
        }
      })
      .catch(() => {});
    return () => {
      live = false;
      if (id) {
        deadStreams.add(id);
        void app.client.browser.screencast.stop({ stream_id: id }).catch(() => {});
      }
    };
  }, [app, sessionId]);

  // The tab strip: refreshed after every agent action, tab switch and control change.
  const refreshTabs = useCallback(() => {
    app.client.browser
      .tabs({ session_id: sessionId })
      .then((r) => setTabs(r.tabs))
      .catch(() => {});
  }, [app, sessionId]);
  useEffect(refreshTabs, [refreshTabs, actions.length, frame?.tab_id, control]);

  // Decode, draw, then ack (one frame in flight; the ack asks for the next one).
  useEffect(() => {
    if (!frame) return;
    let live = true;
    const ack = () => void app.client.browser.screencast.ack({ stream_id: frame.stream_id, frame_seq: frame.frame_seq }).catch(() => {});
    decodeFrame(frame.jpeg_b64)
      .then((img) => {
        const c = canvas.current;
        if (live && c) {
          if (c.width !== img.width) c.width = img.width;
          if (c.height !== img.height) c.height = img.height;
          c.getContext("2d")?.drawImage(img, 0, 0);
        }
        img.close?.();
      })
      .catch(() => {})
      .finally(ack);
    return () => {
      live = false;
    };
  }, [app, frame]);

  // A page opened a file picker during take-over: answer it with the native dialog.
  useEffect(() => {
    if (!chooser || answeredChooser.current === chooser.chooser_id || !inControl) return;
    answeredChooser.current = chooser.chooser_id;
    app.platform
      .pickFiles({ multiple: chooser.multiple, title: t("browser.chooseFiles") })
      .then((files) => app.client.browser.file_chooser.answer({ session_id: sessionId, chooser_id: chooser.chooser_id, files: files ?? [] }))
      .catch(fail)
      .finally(() => keys.current?.focus({ preventScroll: true }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app, sessionId, chooser, inControl]);

  // Keyboard focus goes to the hidden text field while the user is in control.
  useEffect(() => {
    if (inControl) keys.current?.focus({ preventScroll: true });
  }, [inControl]);

  const send = useCallback(
    (event: InputEvent) => {
      if (!inControl || !frame) return;
      void app.client.browser.input({ session_id: sessionId, tab_id: frame.tab_id, event }).catch(() => {});
    },
    [app, sessionId, inControl, frame],
  );

  const point = (e: { clientX: number; clientY: number }) => {
    const c = canvas.current;
    return c && frame ? mapPoint(e.clientX, e.clientY, c.getBoundingClientRect(), frame) : null;
  };

  const mouse = (e: React.MouseEvent<HTMLCanvasElement>, action: "pressed" | "released" | "moved") => {
    const p = point(e);
    if (!p) return;
    const button = action === "moved" ? (e.buttons & 1 ? "left" : e.buttons & 4 ? "middle" : e.buttons & 2 ? "right" : "none") : (MOUSE_BUTTONS[e.button] ?? "left");
    const ev: InputEvent = { type: "mouse", action, x: p.x, y: p.y, button, click_count: action === "moved" ? 0 : Math.max(1, e.detail), modifiers: modifiers(e) };
    if (action !== "moved") return send(ev);
    // Hover: at most one move per animation frame.
    const first = !pendingMove.current;
    pendingMove.current = ev;
    if (first)
      requestAnimationFrame(() => {
        const m = pendingMove.current;
        pendingMove.current = null;
        if (m) send(m);
      });
  };

  // Wheel: a non-passive listener so the panel doesn't scroll while the page does.
  useEffect(() => {
    const c = canvas.current;
    if (!c || !inControl) return;
    const onWheel = (e: WheelEvent) => {
      const p = point(e);
      if (!p) return;
      e.preventDefault();
      const scale = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? c.clientHeight : 1;
      send({ type: "wheel", x: p.x, y: p.y, delta_x: e.deltaX * scale, delta_y: e.deltaY * scale });
    };
    c.addEventListener("wheel", onWheel, { passive: false });
    return () => c.removeEventListener("wheel", onWheel);
  });

  const takeOver = () => void app.client.browser.takeover({ session_id: sessionId }).catch(fail);
  const handBack = () => void app.client.browser.handback({ session_id: sessionId }).catch(fail);
  const popOut = () => {
    setPopping(true);
    app.client.browser
      .popout({ session_id: sessionId })
      .then(refreshTabs)
      .catch(fail)
      .finally(() => setPopping(false));
  };

  if (status && !status.installed.length) return <EmptyState icon="globe" title={t("browser.noneTitle")} hint={t("browser.noneHint")} />;

  const pill = { idle: t("browser.idle"), agent: t("browser.agent"), user: t("browser.user"), handoff: t("browser.handoff"), waiting: t("browser.waiting") }[state];
  const tone = state === "user" || state === "handoff" ? "warning" : state === "agent" || state === "waiting" ? "accent" : "neutral";
  return (
    <div className="browser">
      <div className="browser__bar">
        <Badge tone={tone}>{pill}</Badge>
        <span className="grow" />
        {inControl ? (
          <Button size="sm" onClick={handBack}>
            {t("browser.handBack")}
          </Button>
        ) : (
          <Button size="sm" icon="hand" disabled={!hasTab} onClick={takeOver}>
            {t("browser.takeOver")}
          </Button>
        )}
        <Button size="sm" variant="ghost" icon="stop" disabled={!busy} onClick={() => void app.client.browser.stop({ session_id: sessionId }).catch(fail)}>
          {t("browser.stop")}
        </Button>
        <Button size="sm" variant="ghost" icon="external" disabled={popping} onClick={popOut}>
          {popping ? t("browser.popOutBusy") : t("browser.popOut")}
        </Button>
      </div>
      {state === "handoff" && (
        <Banner tone="warning" icon="hand">
          {control?.reason ? t("browser.handoffReason", { reason: control.reason }) : t("browser.handoff")}
        </Banner>
      )}
      {tabs.length > 1 && (
        <div className="browser__tabs" role="tablist" aria-label="Browser tabs">
          {tabs.map((tb) => (
            <button
              key={tb.tab_id}
              role="tab"
              aria-selected={tb.active}
              className="g-tab"
              title={tb.url}
              onClick={() =>
                void app.client.browser.tab
                  .activate({ session_id: sessionId, tab_id: tb.tab_id })
                  .then(() => setTabs((x) => x.map((y) => ({ ...y, active: y.tab_id === tb.tab_id }))))
                  .catch(fail)
              }
            >
              {tb.title || tb.url || t("browser.newTab")}
            </button>
          ))}
        </div>
      )}
      <div className="browser__view">
        {frame ? (
          <canvas
            ref={canvas}
            className={`browser__canvas ${inControl ? "is-controlled" : ""}`}
            aria-label={pill}
            onMouseDown={(e) => {
              if (!inControl) return;
              e.preventDefault();
              keys.current?.focus({ preventScroll: true });
              mouse(e, "pressed");
            }}
            onMouseUp={(e) => inControl && mouse(e, "released")}
            onMouseMove={(e) => inControl && mouse(e, "moved")}
            onContextMenu={(e) => inControl && e.preventDefault()}
          />
        ) : (
          <EmptyState icon="globe" title={t("browser.idleTitle")} hint={stream && hasTab ? t("browser.loading") : t("browser.idleHint")} />
        )}
        {inControl && (
          <textarea
            ref={keys}
            className="browser__keys"
            aria-label={t("browser.keysLabel")}
            autoCapitalize="off"
            autoComplete="off"
            spellCheck={false}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              if (e.key === "Escape" && !modifiers(e)) {
                e.preventDefault();
                return handBack();
              }
              if (isPasteChord(e)) return;
              e.preventDefault();
              const clip = copyChord(e);
              if (clip && frame) {
                app.client.browser
                  .copy({ session_id: sessionId, tab_id: frame.tab_id })
                  .then((r) => (r.text ? navigator.clipboard?.writeText(r.text) : undefined))
                  .catch(fail);
                // Cut still edits the page; copy needs nothing more from it.
                if (clip === "copy") return;
              }
              send({ type: "key", action: "down", key: e.key, code: e.code, modifiers: modifiers(e), text: keyText(e) });
            }}
            onKeyUp={(e) => {
              if (e.nativeEvent.isComposing || e.key === "Escape" || isPasteChord(e) || copyChord(e) === "copy") return;
              send({ type: "key", action: "up", key: e.key, code: e.code, modifiers: modifiers(e) });
            }}
            onPaste={(e) => {
              e.preventDefault();
              const text = e.clipboardData.getData("text/plain");
              if (text) send({ type: "insert_text", text });
            }}
            onCompositionUpdate={(e) => send({ type: "ime", text: e.data, selection_start: e.data.length, selection_end: e.data.length })}
            onCompositionEnd={(e) => {
              if (e.data) send({ type: "insert_text", text: e.data });
              e.currentTarget.value = "";
            }}
            onInput={(e) => {
              if (!(e.nativeEvent as Event & { isComposing?: boolean }).isComposing) e.currentTarget.value = "";
            }}
          />
        )}
      </div>
      {inControl && <p className="browser__hint">{t("browser.controlHint")}</p>}
      {actions.length > 0 && (
        <details className="browser__steps">
          <summary>
            {t("browser.steps")} ({actions.length})
          </summary>
          <ol>
            {actions.map((a, i) => (
              <li key={i}>
                {a.tool} · {a.origin} · {a.status}
              </li>
            ))}
          </ol>
        </details>
      )}
    </div>
  );
}
