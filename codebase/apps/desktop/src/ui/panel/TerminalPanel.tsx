// Terminal tab (05 §10, D55): one or more interactive terminals per Code session, opened
// in the session folder/worktree. xterm.js (WebGL renderer where available, DOM renderer
// fallback) + fit addon; input/resize go to `terminal.input/resize`; output comes from the
// `Terminals` controller (replay or checkpoint on attach, live `terminal.output` after).
// xterm.js is loaded lazily so the rest of the app (and node tests) never pull it in.
import { useEffect, useRef, useState } from "react";
import { Button, Icon, IconButton } from "@genie/ui";
import { t } from "../../i18n";
import { useEmitter } from "../../state/emitter";
import { MAX_TERMINALS, type TermInfo } from "../../state/terminals";
import { useApp, useAppStatic } from "../context";

export function TerminalPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  useEmitter(app.terminals);
  const list = app.terminals.list(sessionId);
  const [active, setActive] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const current = list.find((x) => x.id === active) ?? list[list.length - 1] ?? null;
  const live = list.filter((x) => !x.exited).length;

  const open = async () => {
    setBusy(true);
    try {
      const info = await app.terminals.open(sessionId, 80, 24);
      setActive(info.id);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };
  const close = (id: string) => void app.terminals.close(id).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));

  if (!list.length)
    return (
      <div className="empty">
        <Icon name="terminal" />
        <p className="muted">{t("terminal.empty")}</p>
        <Button variant="primary" icon="plus" disabled={busy} onClick={() => void open()}>
          {t("terminal.new")}
        </Button>
      </div>
    );

  return (
    <div className="terminal-panel">
      <div className="terminal-tabs" role="tablist" aria-label={t("panel.terminal")}>
        {list.map((x) => (
          <span key={x.id} className={`terminal-tab ${x.id === current?.id ? "is-active" : ""}`}>
            <button role="tab" aria-selected={x.id === current?.id} className="list-button" onClick={() => setActive(x.id)}>
              {x.title}
              {x.exited && <span className="muted"> · {t("terminal.exitedShort")}</span>}
            </button>
            <IconButton icon="x" size="sm" label={t("terminal.close", { title: x.title })} onClick={() => close(x.id)} />
          </span>
        ))}
        <IconButton icon="plus" size="sm" label={t("terminal.new")} disabled={busy || live >= MAX_TERMINALS} onClick={() => void open()} />
      </div>
      {current && <TerminalView key={current.id} info={current} />}
      {current?.exited && (
        <div className="terminal-exit muted" role="status">
          {current.lost ?? (current.code == null ? t("terminal.exitedNoCode") : t("terminal.exited", { code: current.code }))}
          <Button size="sm" onClick={() => void open()} disabled={busy || live >= MAX_TERMINALS}>
            {t("terminal.new")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** Read the theme tokens so the terminal follows light/dark (05 §6). */
function themeFromCss(el: HTMLElement) {
  const cs = getComputedStyle(el);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  return {
    background: v("--code-bg", "#111"),
    foreground: v("--text", "#ddd"),
    cursor: v("--accent", "#2aa"),
    selectionBackground: v("--focus-ring", "#3a6ea5"),
  };
}

function TerminalView({ info }: { info: TermInfo }) {
  const app = useAppStatic();
  const host = useRef<HTMLDivElement>(null);
  const [failed, setFailed] = useState<string | null>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let disposed = false;
    let cleanup = () => {};
    (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit"), import("@xterm/xterm/css/xterm.css")]);
      if (disposed) return;
      const term = new Terminal({
        fontFamily: getComputedStyle(el).getPropertyValue("--font-mono").trim() || "monospace",
        fontSize: 13,
        cursorBlink: !matchMedia?.("(prefers-reduced-motion: reduce)").matches,
        screenReaderMode: true,
        scrollback: 5000,
        theme: themeFromCss(el),
        disableStdin: info.exited,
        convertEol: false,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      try {
        const { WebglAddon } = await import("@xterm/addon-webgl");
        if (!disposed) {
          const gl = new WebglAddon();
          gl.onContextLoss(() => gl.dispose());
          term.loadAddon(gl);
        }
      } catch {
        /* no WebGL: the DOM renderer stays */
      }
      if (disposed) {
        term.dispose();
        return;
      }
      const fitNow = () => {
        try {
          fit.fit();
        } catch {
          /* hidden */
        }
      };
      fitNow();
      const detach = app.terminals.attach(info.id, {
        reset: () => term.reset(),
        write: (bytes) => term.write(bytes),
      });
      void app.terminals.resize(info.id, term.cols, term.rows);
      const onData = term.onData((d) => void app.terminals.input(info.id, d).catch(() => {}));
      const onBinary = term.onBinary((d) => void app.terminals.input(info.id, Uint8Array.from(d, (c) => c.charCodeAt(0))).catch(() => {}));
      const onResize = term.onResize(({ cols, rows }) => void app.terminals.resize(info.id, cols, rows));
      const ro = new ResizeObserver(() => fitNow());
      ro.observe(el);
      term.focus();
      cleanup = () => {
        ro.disconnect();
        onData.dispose();
        onBinary.dispose();
        onResize.dispose();
        detach();
        term.dispose();
      };
    })().catch((e) => setFailed(String(e?.message ?? e)));
    return () => {
      disposed = true;
      cleanup();
    };
  }, [app, info.id, info.exited]);

  if (failed) return <div className="empty muted">{failed}</div>;
  return <div ref={host} className="terminal-host" aria-label={info.title} />;
}
