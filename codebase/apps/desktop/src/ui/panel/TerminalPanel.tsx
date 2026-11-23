// Terminal tab (05 §10, D55): one or more interactive terminals per Code session, opened
// in the session folder/worktree. xterm.js (WebGL renderer where available, DOM renderer
// fallback) + fit addon; input/resize go to `terminal.input/resize`; output comes from the
// `Terminals` controller (replay or checkpoint on attach, live `terminal.output` after).
// xterm.js is loaded lazily so the rest of the app (and node tests) never pull it in.
import { useEffect, useRef, useState } from "react";
import { Button, EmptyState, IconButton } from "@genie/ui";
import { t } from "../../i18n";
import { useEmitter } from "../../state/emitter";
import { MAX_TERMINALS, type TermInfo } from "../../state/terminals";
import { useApp, useAppStatic } from "../context";

const autoOpened = new Set<string>();

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
  // Opening the Terminal tab gives you a shell right away, like the reference; once per session.
  useEffect(() => {
    if (list.length || autoOpened.has(sessionId)) return;
    autoOpened.add(sessionId);
    void open();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);
  const close = (id: string) => void app.terminals.close(id).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));

  if (!list.length)
    return (
      <EmptyState
        icon="terminal"
        title={busy ? t("terminal.starting") : t("terminal.emptyTitle")}
        hint={t("terminal.empty")}
        action={
          <Button icon="plus" disabled={busy} onClick={() => void open()}>
            {t("terminal.new")}
          </Button>
        }
      />
    );

  return (
    <div className="terminal-panel">
      <div className="terminal-bar">
        <div className="terminal-tabs" role="tablist" aria-label={t("panel.terminal")}>
          {list.map((x) => {
            const on = x.id === current?.id;
            return (
              <span key={x.id} className={`terminal-tab${on ? " is-active" : ""}${x.exited ? " is-exited" : ""}`}>
                <button role="tab" aria-selected={on} className="terminal-tab__label" onClick={() => setActive(x.id)} title={x.exited ? `${x.title} · ${t("terminal.exitedShort")}` : x.title}>
                  <span className="terminal-tab__dot" aria-hidden />
                  <span className="terminal-tab__title">{x.title}</span>
                  {x.exited && <span className="g-sr-only"> · {t("terminal.exitedShort")}</span>}
                </button>
                <IconButton className="terminal-tab__close" icon="x" size="sm" tooltip={false} label={t("terminal.close", { title: x.title })} onClick={() => close(x.id)} />
              </span>
            );
          })}
        </div>
        <IconButton icon="plus" size="sm" label={t("terminal.new")} disabled={busy || live >= MAX_TERMINALS} onClick={() => void open()} />
      </div>
      <div className="terminal-body">{current && <TerminalView key={current.id} info={current} />}</div>
      {current?.exited && (
        <div className="terminal-exit" role="status">
          <span className="terminal-exit__text">
            {current.lost ?? (current.code == null ? t("terminal.exitedNoCode") : t("terminal.exited", { code: current.code }))}
          </span>
          <Button size="sm" icon="refresh" onClick={() => void open()} disabled={busy || live >= MAX_TERMINALS}>
            {t("terminal.new")}
          </Button>
        </div>
      )}
    </div>
  );
}

// ANSI palettes tuned for each theme: xterm's defaults assume a dark background, so yellow,
// cyan and white wash out on the light theme.
const ANSI_LIGHT = {
  black: "#24292f", red: "#c4314b", green: "#1a7f37", yellow: "#9a6700", blue: "#0b63c4", magenta: "#8250df", cyan: "#137a80", white: "#6e7781",
  brightBlack: "#57606a", brightRed: "#d1242f", brightGreen: "#2da44e", brightYellow: "#bf8700", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#1b9aa0", brightWhite: "#8c959f",
};
const ANSI_DARK = {
  black: "#3b3f45", red: "#f2777a", green: "#8fd18b", yellow: "#e9c46a", blue: "#6cb6ff", magenta: "#c49bff", cyan: "#5fd1d1", white: "#c9d1d9",
  brightBlack: "#6e7681", brightRed: "#ff9492", brightGreen: "#a8e6a3", brightYellow: "#f5d76e", brightBlue: "#91cbff", brightMagenta: "#dcbdfb", brightCyan: "#8ce6e6", brightWhite: "#f0f3f6",
};

function isDark() {
  const attr = document.documentElement.dataset.theme;
  return attr ? attr === "dark" : !!matchMedia?.("(prefers-color-scheme: dark)").matches;
}

/** Read the theme tokens so the terminal follows light/dark (05 §6) and blends into the panel. */
function themeFromCss(el: HTMLElement) {
  const cs = getComputedStyle(el);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  const bg = v("--bg", "#fff");
  return {
    ...(isDark() ? ANSI_DARK : ANSI_LIGHT),
    background: bg,
    foreground: v("--text", "#222"),
    cursor: v("--accent", "#2aa"),
    cursorAccent: bg,
    selectionBackground: v("--accent-5", "#b6e3e6"),
    scrollbarSliderBackground: "rgba(128,128,128,0.25)",
    scrollbarSliderHoverBackground: "rgba(128,128,128,0.4)",
    scrollbarSliderActiveBackground: "rgba(128,128,128,0.5)",
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
        lineHeight: 1.25,
        fontWeightBold: "600",
        cursorStyle: "bar",
        cursorWidth: 2,
        cursorInactiveStyle: "outline",
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
      // Follow Appearance changes while the terminal is open.
      const mo = new MutationObserver(() => (term.options.theme = themeFromCss(el)));
      mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
      term.focus();
      cleanup = () => {
        ro.disconnect();
        mo.disconnect();
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
