// Browser tab (05 §9, D56): screencast frames on a <canvas>, agent-control banner, tab
// strip, take over (input forwarded with device-scale mapping), hand back, stop, pop out.
import { useEffect, useRef, useState } from "react";
import type { BrowserStatus, InputEvent, TabInfo } from "@genie/protocol";
import { Badge, Button, Icon } from "@genie/ui";
import { t } from "../../i18n";
import { useApp, useSession } from "../context";

/** CSS pixels on the canvas → page coordinates (SA-24 mapping). */
export function mapPoint(clientX: number, clientY: number, rect: { left: number; top: number; width: number; height: number }, frame: { w: number; h: number; device_scale: number }) {
  const sx = frame.w / rect.width;
  const sy = frame.h / rect.height;
  return { x: ((clientX - rect.left) * sx) / frame.device_scale, y: ((clientY - rect.top) * sy) / frame.device_scale };
}

export function modifiers(e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): number {
  return (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
}

export function BrowserPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const store = useSession(sessionId);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [status, setStatus] = useState<BrowserStatus | null>(null);
  const [tabs, setTabs] = useState<TabInfo[]>([]);
  const [stream, setStream] = useState<string | null>(null);
  const frame = store?.model.browser.frame ?? null;
  const control = store?.model.browser.control?.control ?? (frame ? "agent" : null);
  const actions = store?.model.browser.actions ?? [];
  const inControl = control === "user";
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });

  useEffect(() => {
    app.client.browser.status({}).then(setStatus).catch(() => setStatus(null));
  }, [app]);

  // Screencast while the tab is visible (SA-23: JPEG 60, ≤ 1280×800).
  useEffect(() => {
    let id: string | null = null;
    let live = true;
    app.client.browser.screencast
      .start({ session_id: sessionId, quality: 60, max_width: 1280, max_height: 800 })
      .then((r) => {
        id = r.stream_id;
        if (live) setStream(r.stream_id);
        else void app.client.browser.screencast.stop({ stream_id: r.stream_id });
      })
      .catch(() => {});
    app.client.browser.tabs({ session_id: sessionId }).then((r) => setTabs(r.tabs)).catch(() => {});
    return () => {
      live = false;
      if (id) void app.client.browser.screencast.stop({ stream_id: id }).catch(() => {});
    };
  }, [app, sessionId]);

  // Draw + ack (one frame in flight).
  useEffect(() => {
    if (!frame || !canvas.current) return;
    const img = new Image();
    img.onload = () => {
      const c = canvas.current;
      if (!c) return;
      c.width = frame.w;
      c.height = frame.h;
      c.getContext("2d")?.drawImage(img, 0, 0);
    };
    img.src = `data:image/jpeg;base64,${frame.jpeg_b64}`;
    void app.client.browser.screencast.ack({ stream_id: frame.stream_id, frame_seq: frame.frame_seq }).catch(() => {});
  }, [app, frame]);

  const send = (event: InputEvent) => {
    if (!inControl || !frame) return;
    void app.client.browser.input({ session_id: sessionId, tab_id: frame.tab_id, event }).catch(() => {});
  };
  const mouse = (e: React.MouseEvent<HTMLCanvasElement>, action: string) => {
    if (!frame) return;
    const p = mapPoint(e.clientX, e.clientY, e.currentTarget.getBoundingClientRect(), frame);
    send({ type: "mouse", action, x: p.x, y: p.y, button: ["left", "middle", "right"][e.button] ?? "left", click_count: e.detail, modifiers: modifiers(e) });
  };

  if (status && !status.installed.length)
    return (
      <div className="empty">
        <Icon name="globe" />
        <p>{t("browser.none")}</p>
      </div>
    );

  const pill = control === "user" ? t("browser.user") : control === "waiting_for_approval" ? t("browser.waiting") : control ? t("browser.agent") : t("browser.idle");
  return (
    <div className="browser">
      <div className="browser__bar">
        <Badge tone={control === "user" ? "warning" : control ? "accent" : "neutral"}>{pill}</Badge>
        <span className="grow" />
        {inControl ? (
          <Button size="sm" onClick={() => void app.client.browser.handback({ session_id: sessionId }).catch(fail)}>
            {t("browser.handBack")}
          </Button>
        ) : (
          <Button size="sm" icon="hand" disabled={!frame} onClick={() => void app.client.browser.takeover({ session_id: sessionId }).catch(fail)}>
            {t("browser.takeOver")}
          </Button>
        )}
        <Button size="sm" variant="ghost" icon="stop" disabled={!frame} onClick={() => void app.client.browser.stop({ session_id: sessionId }).catch(fail)}>
          {t("browser.stop")}
        </Button>
        <Button size="sm" variant="ghost" icon="external" disabled={!frame} onClick={() => void app.client.browser.popout({ session_id: sessionId }).catch(fail)}>
          {t("browser.popOut")}
        </Button>
      </div>
      {tabs.length > 1 && (
        <div className="browser__tabs" role="tablist" aria-label="Browser tabs">
          {tabs.map((tb) => (
            <button
              key={tb.tab_id}
              role="tab"
              aria-selected={tb.active}
              className="g-tab"
              onClick={() => void app.client.browser.tab.activate({ session_id: sessionId, tab_id: tb.tab_id }).then(() => setTabs((x) => x.map((y) => ({ ...y, active: y.tab_id === tb.tab_id }))))}
            >
              {tb.title || tb.url}
            </button>
          ))}
        </div>
      )}
      {frame || stream ? (
        <canvas
          ref={canvas}
          className={`browser__canvas ${inControl ? "is-controlled" : ""}`}
          tabIndex={inControl ? 0 : -1}
          aria-label={pill}
          onMouseDown={(e) => mouse(e, "down")}
          onMouseUp={(e) => mouse(e, "up")}
          onMouseMove={(e) => e.buttons && mouse(e, "move")}
          onWheel={(e) => {
            if (!frame) return;
            const p = mapPoint(e.clientX, e.clientY, e.currentTarget.getBoundingClientRect(), frame);
            send({ type: "wheel", x: p.x, y: p.y, delta_x: e.deltaX, delta_y: e.deltaY });
          }}
          onKeyDown={(e) => {
            if (!inControl) return;
            if (e.key === "Escape") return void app.client.browser.handback({ session_id: sessionId }).catch(fail);
            e.preventDefault();
            send({ type: "key", action: "down", key: e.key, code: e.code, modifiers: modifiers(e), text: e.key.length === 1 ? e.key : null });
          }}
          onKeyUp={(e) => inControl && send({ type: "key", action: "up", key: e.key, code: e.code, modifiers: modifiers(e) })}
          onPaste={(e) => {
            const text = e.clipboardData.getData("text");
            if (text) send({ type: "insert_text", text });
          }}
        />
      ) : (
        <div className="empty muted">{t("browser.idle")}</div>
      )}
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
