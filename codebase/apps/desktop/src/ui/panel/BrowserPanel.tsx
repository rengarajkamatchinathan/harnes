// Browser tab (05 §9, D56, t47): screencast frames on a <canvas>, agent-control banner, tab
// strip with new/close, address bar with back/forward/reload, take over (input forwarded in
// CSS pixels), hand back, stop, pop out, ⋮ menu. While Genie is idle the user can use the
// page directly; during a turn they take over first.
//
// Like a browser: the page is laid out at the panel's size (`browser.viewport`), with zoom (Ctrl +/−/0, Ctrl+wheel), find in page (Ctrl+F), the
// page's mouse cursor and hovered link, and a right-click menu unless the page has its own.
//
// The screencast stream watches the *session*: it is started once when the tab shows and
// the daemon attaches it to whichever tab the agent has active (including the first one,
// opened after the panel), so the panel never has to restart it.
import { useCallback, useEffect, useRef, useState } from "react";
import type { BrowserControl, BrowserStatus, FindResult, HitResult, InputEvent, InspectResult, TabInfo } from "@genie/protocol";
import { Badge, Banner, Button, EmptyState, IconButton, Input, Menu, type MenuItem } from "@genie/ui";
import { plural, t } from "../../i18n";
import { useEmitter } from "../../state/emitter";
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

/** The composer text for a picked element (Element select, ticket 47). */
export function elementMessage(r: Pick<InspectResult, "url" | "selector" | "text" | "html">): string {
  const text = r.text.length > 200 ? `${r.text.slice(0, 199)}…` : r.text;
  const lines = [`Selected element on ${r.url}`, `Selector: \`${r.selector}\``];
  if (text) lines.push(`Text: "${text}"`);
  lines.push("```html", r.html, "```");
  return lines.join("\n");
}

function jpegFile(b64: string, name: string): File {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new File([bytes], name, { type: "image/jpeg" });
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

/** Browser shortcuts the panel handles itself instead of sending them to the page. */
export type BrowserChord = "find" | "zoomIn" | "zoomOut" | "zoomReset";
export function browserChord(e: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean }): BrowserChord | null {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return null;
  switch (e.key) {
    case "f":
    case "F":
      return "find";
    case "=":
    case "+":
      return "zoomIn";
    case "-":
    case "_":
      return "zoomOut";
    case "0":
      return "zoomReset";
    default:
      return null;
  }
}

/** Chrome's zoom levels. */
export const ZOOM_STEPS = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3];

/** The next zoom level in direction `dir` (0 resets to 100%). */
export function stepZoom(z: number, dir: 1 | -1 | 0): number {
  if (dir === 0) return 1;
  if (dir > 0) return ZOOM_STEPS.find((s) => s > z + 1e-6) ?? ZOOM_STEPS[ZOOM_STEPS.length - 1];
  return [...ZOOM_STEPS].reverse().find((s) => s < z - 1e-6) ?? ZOOM_STEPS[0];
}

/**
 * The page viewport for a panel of `w`×`h` CSS px: the page is laid out at the panel's size
 * divided by the zoom, so it fills the panel and zoom works like Chrome's. Scale 1: headless
 * Chrome's screencast captures at 1× whatever device scale is emulated, so a higher one
 * would only make pages fetch HiDPI images for nothing.
 */
export function viewportFor(w: number, h: number, zoom: number) {
  return { width: Math.floor(w / zoom), height: Math.floor(h / zoom), device_scale: 1 };
}

const CURSORS = new Set(
  "auto default none context-menu help pointer progress wait cell crosshair text vertical-text alias copy move no-drop not-allowed grab grabbing all-scroll col-resize row-resize n-resize e-resize s-resize w-resize ne-resize nw-resize se-resize sw-resize ew-resize ns-resize nesw-resize nwse-resize zoom-in zoom-out".split(" "),
);

/** A page's CSS cursor as a keyword the canvas can show (`url(...)` images fall back to theirs). */
export function cssCursor(c: string): string {
  const k = c.split(",").pop()?.trim() ?? "";
  return CURSORS.has(k) ? k : "default";
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

/** The address bar's text for a tab URL (blank for a new tab). */
export function addressText(url: string | undefined): string {
  return !url || url === "about:blank" ? "" : url;
}

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
  const [addr, setAddr] = useState("");
  const editing = useRef(false);
  const address = useRef<HTMLInputElement>(null);
  const lastTabs = useRef(0);
  const view = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);
  const [zoom, setZoom] = useState(1);
  const zoomBy = (d: 1 | -1 | 0) => setZoom((z) => stepZoom(z, d));
  const lastWheelZoom = useRef(0);
  // The page's cursor and hovered link (`browser.hit`, one probe in flight, trailing).
  const [cursor, setCursor] = useState("default");
  const [linkHover, setLinkHover] = useState<string | null>(null);
  const probing = useRef(false);
  const probeAt = useRef<{ x: number; y: number; tab: string } | null>(null);
  // Right-click menu at a point of the view, with what was under the pointer.
  const [ctx, setCtx] = useState<{ x: number; y: number; hit: HitResult } | null>(null);
  // Find in page.
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<FindResult | null>(null);
  const findInput = useRef<HTMLInputElement>(null);

  // The page fills the panel: its size (settled), with the zoom folded in.
  useEffect(() => {
    const el = view.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ro = new ResizeObserver(([e]) => {
      const { width, height } = e.contentRect;
      clearTimeout(timer);
      timer = setTimeout(() => setSize({ w: width, h: height }), 120);
    });
    ro.observe(el);
    return () => {
      clearTimeout(timer);
      ro.disconnect();
    };
  }, []);
  useEffect(() => {
    if (!size) return;
    void app.client.browser.viewport({ session_id: sessionId, ...viewportFor(size.w, size.h, zoom) }).catch(() => {});
  }, [app, sessionId, size, zoom]);

  const model = store?.model;
  const raw = model?.browser.frame ?? null;
  const frame = raw && !deadStreams.has(raw.stream_id) ? raw : null;
  const control = model?.browser.control ?? null;
  const chooser = model?.browser.fileChooser ?? null;
  const actions = model?.browser.actions ?? [];
  const busy = model ? isBusy(model) : false;
  const state = bannerState(control, busy);
  const inControl = state === "user" || state === "handoff";
  // Idle: the page is the user's to use directly (the daemon doesn't pause anything).
  const interactive = inControl || state === "idle";
  const hasTab = tabs.length > 0 || !!frame;
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  // Element select (Cmd/Ctrl+Shift+S): hover outlines an element, a click adds it to the message.
  const selecting = app.selectingElement && !!frame;
  const [hover, setHover] = useState<InspectResult | null>(null);
  const inspecting = useRef(false);
  useEffect(() => {
    if (!app.selectingElement) setHover(null);
    if (!app.selectingElement) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && app.setSelectingElement(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [app, app.selectingElement]);

  useEffect(() => {
    app.client.browser.status({}).then(setStatus).catch(() => setStatus(null));
  }, [app]);
  // ⋮ Keep cookies (t47): a user setting the daemon reads when it quits.
  const [keepCookies, setKeepCookies] = useState(true);
  useEffect(() => {
    app.client.config
      .get({ keys: ["browser.keepCookies"] })
      .then((r) => setKeepCookies(r.values["browser.keepCookies"] !== false))
      .catch(() => {});
  }, [app]);

  // One stream per visible tab; it follows the agent's active tab. Frames come at the
  // viewport's size (above), up to 2× SA-23's 1280×800 for a large panel.
  useEffect(() => {
    let id: string | null = null;
    let live = true;
    app.client.browser.screencast
      .start({ session_id: sessionId, quality: 75, max_width: 2560, max_height: 1600 })
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
    lastTabs.current = Date.now();
    app.client.browser
      .tabs({ session_id: sessionId })
      .then((r) => setTabs(r.tabs))
      .catch(() => {});
  }, [app, sessionId]);
  useEffect(refreshTabs, [refreshTabs, actions.length, frame?.tab_id, control]);
  // A closed popped-out window (or a crashed browser) sends no frames: poll so the daemon
  // notices and re-opens the tabs here.
  useEffect(() => {
    const id = setInterval(refreshTabs, 3000);
    return () => clearInterval(id);
  }, [refreshTabs]);
  // Pages change under links and redirects: refresh at most every 1.5 s while frames arrive.
  useEffect(() => {
    if (frame && Date.now() - lastTabs.current > 1500) refreshTabs();
  }, [frame, refreshTabs]);
  const activeTab = tabs.find((x) => x.active);
  // The address bar follows the shown tab unless the user is typing in it.
  useEffect(() => {
    if (!editing.current) setAddr(addressText(activeTab?.url));
  }, [activeTab?.url, activeTab?.tab_id]);
  /** After a navigation: refresh now and once the page has committed. */
  const settle = () => {
    refreshTabs();
    setTimeout(refreshTabs, 800);
  };
  const go = (url: string) => {
    const u = url.trim();
    if (!u) return;
    editing.current = false;
    address.current?.blur();
    void app.client.browser
      .navigate({ session_id: sessionId, url: u })
      .then(settle)
      .catch(fail);
  };
  const step = (action: "back" | "forward" | "reload") =>
    void app.client.browser.navigate({ session_id: sessionId, action }).then(settle).catch(fail);
  const newTab = () =>
    void app.client.browser
      .tab.new({ session_id: sessionId })
      .then(() => {
        settle();
        editing.current = true;
        setAddr("");
        address.current?.focus();
      })
      .catch(fail);
  const closeTab = (tabId: string) =>
    void app.client.browser.tab
      .close({ session_id: sessionId, tab_id: tabId })
      .then(() => setTabs((x) => x.filter((y) => y.tab_id !== tabId)))
      .then(settle)
      .catch(fail);

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
      if (!interactive || !frame) return;
      void app.client.browser.input({ session_id: sessionId, tab_id: frame.tab_id, event }).catch(() => {});
    },
    [app, sessionId, interactive, frame],
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

  const hoverAt = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const p = point(e);
    if (!p || !frame || inspecting.current) return;
    inspecting.current = true;
    app.client.browser
      .inspect({ session_id: sessionId, tab_id: frame.tab_id, x: p.x, y: p.y, capture: false })
      .then(setHover)
      .catch(() => setHover(null))
      .finally(() => (inspecting.current = false));
  };
  const pickAt = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const p = point(e);
    if (!p || !frame) return;
    app.client.browser
      .inspect({ session_id: sessionId, tab_id: frame.tab_id, x: p.x, y: p.y, capture: true })
      .then((r) => {
        const files = r.image_b64 ? [jpegFile(r.image_b64, `${r.tag}-element.jpg`)] : [];
        app.injectComposer(sessionId, elementMessage(r), files);
        app.setSelectingElement(false);
        app.toast({ title: t("browser.elementAdded"), tone: "success" });
      })
      .catch(fail);
  };
  const box = (() => {
    const c = canvas.current;
    if (!selecting || !hover || !c || !frame) return null;
    const sx = c.clientWidth / frame.w;
    const sy = c.clientHeight / frame.h;
    return { left: c.offsetLeft + hover.x * sx, top: c.offsetTop + hover.y * sy, width: hover.w * sx, height: hover.h * sy };
  })();

  // The page's cursor and hovered link under the pointer.
  const probe = () => {
    const at = probeAt.current;
    probeAt.current = null;
    if (!at || probing.current) return;
    probing.current = true;
    app.client.browser
      .hit({ session_id: sessionId, tab_id: at.tab, x: at.x, y: at.y })
      .then((h) => {
        setCursor(cssCursor(h.cursor));
        setLinkHover(h.link ?? null);
      })
      .catch(() => {})
      .finally(() => {
        probing.current = false;
        if (probeAt.current) setTimeout(probe, 50);
      });
  };
  const probeLater = (e: { clientX: number; clientY: number }) => {
    const p = point(e);
    if (!p || !frame) return;
    probeAt.current = { ...p, tab: frame.tab_id };
    if (!probing.current) probe();
  };

  const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);
  const cmdKey = isMac ? "⌘" : "Ctrl+";
  const copySelection = () => {
    if (!frame) return;
    app.client.browser
      .copy({ session_id: sessionId, tab_id: frame.tab_id })
      .then((r) => (r.text ? navigator.clipboard?.writeText(r.text) : undefined))
      .catch(fail);
  };
  const pasteClipboard = () => {
    keys.current?.focus({ preventScroll: true });
    if (!navigator.clipboard?.readText) return app.toast({ title: t("browser.pasteBlocked", { key: `${cmdKey}V` }), tone: "info" });
    navigator.clipboard
      .readText()
      .then((text) => text && send({ type: "insert_text", text }))
      .catch(() => app.toast({ title: t("browser.pasteBlocked", { key: `${cmdKey}V` }), tone: "info" }));
  };
  const selectAll = () => {
    keys.current?.focus({ preventScroll: true });
    const mods = isMac ? 4 : 2;
    send({ type: "key", action: "down", key: "a", code: "KeyA", modifiers: mods, text: null });
    send({ type: "key", action: "up", key: "a", code: "KeyA", modifiers: mods });
  };

  // Find in page: a new query searches from the top (debounced); Enter / Shift+Enter step.
  const runFind = (q: string, opts: { backwards?: boolean; restart?: boolean } = {}) => {
    if (!frame || !q) return;
    app.client.browser
      .find({ session_id: sessionId, tab_id: frame.tab_id, query: q, backwards: !!opts.backwards, restart: !!opts.restart })
      .then(setFound)
      .catch(fail);
  };
  useEffect(() => {
    if (!finding) return;
    if (!query) return setFound(null);
    const id = setTimeout(() => runFind(query, { restart: true }), 200);
    return () => clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finding, query]);
  const openFind = () => {
    if (!frame || !interactive) return;
    setFinding(true);
    setTimeout(() => {
      findInput.current?.focus();
      findInput.current?.select();
    });
  };
  const closeFind = () => {
    setFinding(false);
    setFound(null);
    keys.current?.focus({ preventScroll: true });
  };
  const onChord = (c: BrowserChord) => {
    if (c === "find") openFind();
    else zoomBy(c === "zoomIn" ? 1 : c === "zoomOut" ? -1 : 0);
  };

  // Right-click: the click already went to the page; ours opens unless the page showed its own.
  const openMenu = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const p = point(e);
    const v = view.current;
    if (!p || !frame || !v) return;
    const r = v.getBoundingClientRect();
    const at = { x: e.clientX - r.left, y: e.clientY - r.top };
    const tab = frame.tab_id;
    setTimeout(
      () =>
        void app.client.browser
          .hit({ session_id: sessionId, tab_id: tab, x: p.x, y: p.y })
          .then((hit) => !hit.page_menu && setCtx({ ...at, hit }))
          .catch(() => setCtx({ ...at, hit: { cursor: "default", editable: false, page_menu: false } })),
      80,
    );
  };
  const menuItems = (hit: HitResult): MenuItem[] => {
    const link = hit.link;
    const items: MenuItem[] = [];
    if (link)
      items.push(
        {
          id: "open-link",
          icon: "plus",
          label: t("browser.openLinkNewTab"),
          onSelect: () => void app.client.browser.tab.new({ session_id: sessionId, url: link }).then(settle).catch(fail),
        },
        { id: "copy-link", icon: "copy", label: t("browser.copyLink"), onSelect: () => void navigator.clipboard?.writeText(link) },
      );
    items.push(
      { id: "back", icon: "chevronLeft", label: t("browser.back"), separatorBefore: !!link, onSelect: () => step("back") },
      { id: "forward", icon: "chevronRight", label: t("browser.forward"), onSelect: () => step("forward") },
      { id: "reload", icon: "refresh", label: t("browser.reload"), onSelect: () => step("reload") },
      { id: "copy", icon: "copy", label: t("browser.copy"), shortcut: `${cmdKey}C`, separatorBefore: true, onSelect: copySelection },
    );
    if (hit.editable) items.push({ id: "paste", label: t("browser.paste"), shortcut: `${cmdKey}V`, onSelect: pasteClipboard });
    items.push(
      { id: "select-all", label: t("browser.selectAll"), shortcut: `${cmdKey}A`, onSelect: selectAll },
      { id: "find", icon: "search", label: t("browser.findInPage"), shortcut: `${cmdKey}F`, separatorBefore: true, keepFocus: true, onSelect: openFind },
      { id: "zoom-in", label: t("browser.zoomIn"), shortcut: `${cmdKey}+`, onSelect: () => zoomBy(1) },
      { id: "zoom-out", label: t("browser.zoomOut"), shortcut: `${cmdKey}−`, onSelect: () => zoomBy(-1) },
    );
    if (zoom !== 1) items.push({ id: "zoom-reset", label: t("browser.zoomReset"), shortcut: `${cmdKey}0`, onSelect: () => zoomBy(0) });
    items.push({ id: "inspect", icon: "search", label: t("browser.selectElement"), separatorBefore: true, onSelect: () => app.setSelectingElement(true) });
    return items;
  };

  // Wheel: a non-passive listener so the panel doesn't scroll while the page does.
  // Ctrl+wheel (and trackpad pinch) zooms, one step per 150 ms.
  useEffect(() => {
    const c = canvas.current;
    if (!c || !interactive) return;
    const onWheel = (e: WheelEvent) => {
      const p = point(e);
      if (!p) return;
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) {
        if (e.deltaY && Date.now() - lastWheelZoom.current > 150) {
          lastWheelZoom.current = Date.now();
          zoomBy(e.deltaY < 0 ? 1 : -1);
        }
        return;
      }
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
    <div
      className="browser"
      onKeyDown={(e) => {
        const c = browserChord(e);
        if (!c || !hasTab) return;
        e.preventDefault();
        onChord(c);
      }}
    >
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
        <IconButton
          icon="search"
          size="sm"
          className={app.selectingElement ? "is-on" : ""}
          aria-pressed={app.selectingElement}
          disabled={!frame}
          label={t("browser.selectElement")}
          shortcut={/Mac/i.test(navigator.platform) ? "⌘⇧S" : "Ctrl+Shift+S"}
          onClick={() => app.setSelectingElement(!app.selectingElement)}
        />
        {app.ui.mode === "code" && <DevServersMenu sessionId={sessionId} />}
        <Menu
          label={t("browser.more")}
          align="right"
          trigger={(p) => <IconButton {...p} icon="more" size="sm" label={t("browser.more")} />}
          items={[
            { id: "find", icon: "search", label: t("browser.findInPage"), shortcut: `${cmdKey}F`, disabled: !frame || !interactive, keepFocus: true, onSelect: openFind },
            { id: "zoom-in", label: t("browser.zoomIn"), shortcut: `${cmdKey}+`, disabled: !hasTab, onSelect: () => zoomBy(1) },
            { id: "zoom-out", label: t("browser.zoomOut"), shortcut: `${cmdKey}−`, disabled: !hasTab, onSelect: () => zoomBy(-1) },
            {
              id: "cookies",
              label: t("browser.keepCookies"),
              separatorBefore: true,
              hint: t("browser.keepCookiesHint"),
              checked: keepCookies,
              onSelect: () => {
                const v = !keepCookies;
                app.client.config
                  .set({ key: "browser.keepCookies", value: v, scope: "user" })
                  .then(() => setKeepCookies(v))
                  .catch(fail);
              },
            },
            {
              id: "links",
              label: t("browser.openLinksHere"),
              checked: app.ui.browserOpenLinks,
              onSelect: () => app.setUi({ browserOpenLinks: !app.ui.browserOpenLinks, browserLinkAsked: true }),
            },
            ...(app.ui.mode === "code"
              ? [
                  {
                    id: "verify",
                    label: t("preview.autoVerify"),
                    hint: t("preview.autoVerifyHint"),
                    checked: app.devServers.get(sessionId).auto_verify,
                    onSelect: () => void app.devServers.setAutoVerify(sessionId, !app.devServers.get(sessionId).auto_verify).catch(fail),
                  },
                ]
              : []),
            {
              id: "clear",
              label: t("browser.clearData"),
              icon: "trash",
              separatorBefore: true,
              onSelect: () =>
                void app.client.browser.data
                  .clear({ what: "all" })
                  .then(() => app.toast({ title: t("browser.cleared"), tone: "success" }))
                  .catch(fail),
            },
          ]}
        />
      </div>
      {selecting && (
        <Banner tone="info" icon="search">
          {t("browser.selecting")}
        </Banner>
      )}
      {state === "handoff" && (
        <Banner tone="warning" icon="hand">
          {control?.reason ? t("browser.handoffReason", { reason: control.reason }) : t("browser.handoff")}
        </Banner>
      )}
      {tabs.length > 0 && (
        <div className="browser__tabs" role="tablist" aria-label={t("browser.tabsLabel")}>
          {tabs.map((tb) => (
            <div key={tb.tab_id} className="browser__tab" data-active={tb.active}>
              <button
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
                {tb.title || addressText(tb.url) || t("browser.newTab")}
              </button>
              <IconButton className="browser__tab-close" icon="x" size="sm" label={t("browser.closeTab")} tooltip={false} onClick={() => closeTab(tb.tab_id)} />
            </div>
          ))}
          <IconButton icon="plus" size="sm" label={t("browser.newTabButton")} onClick={newTab} />
        </div>
      )}
      <form
        className="browser__nav"
        onSubmit={(e) => {
          e.preventDefault();
          go(addr);
        }}
      >
        <IconButton icon="chevronLeft" size="sm" label={t("browser.back")} disabled={!activeTab} onClick={() => step("back")} />
        <IconButton icon="chevronRight" size="sm" label={t("browser.forward")} disabled={!activeTab} onClick={() => step("forward")} />
        <IconButton icon="refresh" size="sm" label={t("browser.reload")} disabled={!activeTab} onClick={() => step("reload")} />
        <Input
          ref={address}
          className="browser__address"
          aria-label={t("browser.address")}
          placeholder={t("browser.addressPlaceholder")}
          value={addr}
          spellCheck={false}
          autoCapitalize="off"
          autoComplete="off"
          onFocus={(e) => {
            editing.current = true;
            e.currentTarget.select();
          }}
          onBlur={() => {
            editing.current = false;
            setAddr(addressText(activeTab?.url));
          }}
          onChange={(e) => setAddr(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") e.currentTarget.blur();
          }}
        />
        {zoom !== 1 && (
          <Button size="sm" variant="ghost" title={t("browser.zoomReset")} onClick={() => zoomBy(0)}>
            {Math.round(zoom * 100)}%
          </Button>
        )}
      </form>
      {finding && (
        <div className="browser__find" role="search">
          <Input
            ref={findInput}
            className="browser__find-input"
            aria-label={t("browser.findInPage")}
            placeholder={t("browser.findPlaceholder")}
            value={query}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                runFind(query, { backwards: e.shiftKey });
              } else if (e.key === "Escape") {
                e.preventDefault();
                closeFind();
              }
            }}
          />
          <span className="browser__find-count" aria-live="polite">
            {found && query ? (found.matches ? plural(found.matches, { one: t("browser.findOne"), other: t("browser.findMany") }) : t("browser.findNone")) : ""}
          </span>
          <IconButton icon="arrowUp" size="sm" label={t("browser.findPrev")} disabled={!found?.matches} onClick={() => runFind(query, { backwards: true })} />
          <IconButton icon="arrowDown" size="sm" label={t("browser.findNext")} disabled={!found?.matches} onClick={() => runFind(query)} />
          <IconButton icon="x" size="sm" label={t("browser.findClose")} onClick={closeFind} />
        </div>
      )}
      <div className="browser__view" ref={view}>
        {frame ? (
          <canvas
            ref={canvas}
            className={`browser__canvas ${inControl ? "is-controlled" : ""} ${selecting ? "is-selecting" : ""}`}
            aria-label={pill}
            style={interactive && !selecting ? { cursor } : undefined}
            onClick={(e) => selecting && pickAt(e)}
            onMouseDown={(e) => {
              if (selecting) return e.preventDefault();
              if (!interactive) return;
              e.preventDefault();
              keys.current?.focus({ preventScroll: true });
              mouse(e, "pressed");
            }}
            onMouseUp={(e) => !selecting && interactive && mouse(e, "released")}
            onMouseMove={(e) => {
              if (selecting) return hoverAt(e);
              if (!interactive) return;
              mouse(e, "moved");
              probeLater(e);
            }}
            onMouseLeave={() => {
              setHover(null);
              setLinkHover(null);
            }}
            onContextMenu={(e) => {
              if (!interactive) return;
              e.preventDefault();
              if (!selecting) openMenu(e);
            }}
          />
        ) : (
          <EmptyState icon="globe" title={t("browser.idleTitle")} hint={stream && hasTab ? t("browser.loading") : t("browser.idleHint")} />
        )}
        {linkHover && !ctx && interactive && !selecting && <div className="browser__status">{linkHover}</div>}
        {ctx && (
          <div className="browser__ctx" style={{ left: ctx.x, top: ctx.y }}>
            <Menu
              label={t("browser.pageMenu")}
              open
              onOpenChange={(o) => !o && setCtx(null)}
              items={menuItems(ctx.hit)}
              trigger={(p) => <span {...p} className="browser__ctx-anchor" />}
            />
          </div>
        )}
        {box && hover && (
          <div className="browser__pick" style={box} aria-hidden>
            <span className="browser__pick-label">
              {hover.tag} · {Math.round(hover.w)}×{Math.round(hover.h)}
            </span>
          </div>
        )}
        {interactive && frame && (
          <textarea
            ref={keys}
            className="browser__keys"
            aria-label={t("browser.keysLabel")}
            autoCapitalize="off"
            autoComplete="off"
            spellCheck={false}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing || e.keyCode === 229) return;
              // Esc hands back during take-over; while idle it goes to the page.
              if (inControl && e.key === "Escape" && !modifiers(e)) {
                e.preventDefault();
                return handBack();
              }
              // Find and zoom are the panel's (handled where the event bubbles to).
              if (isPasteChord(e) || browserChord(e)) return;
              e.preventDefault();
              const clip = copyChord(e);
              if (clip && frame) {
                copySelection();
                // Cut still edits the page; copy needs nothing more from it.
                if (clip === "copy") return;
              }
              send({ type: "key", action: "down", key: e.key, code: e.code, modifiers: modifiers(e), text: keyText(e) });
            }}
            onKeyUp={(e) => {
              if (e.nativeEvent.isComposing || (inControl && e.key === "Escape") || isPasteChord(e) || browserChord(e) || copyChord(e) === "copy") return;
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

/**
 * **Dev servers** (ticket 47): each `.genie/launch.json` entry and started server with
 * Open / Stop or Start, then Stop all. Starting one opens it in this tab once it listens
 * (`preview.changed`, handled in the app state).
 */
function DevServersMenu({ sessionId }: { sessionId: string }) {
  const app = useApp();
  useEmitter(app.devServers);
  useEffect(() => app.devServers.ensure(sessionId), [app, sessionId]);
  const list = app.devServers.get(sessionId);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const names = [...new Set([...list.configs, ...list.servers.map((s) => s.name)])];
  const live = list.servers.filter((s) => s.state === "running" || s.state === "starting");
  const items: MenuItem[] = [];
  for (const name of names) {
    const s = list.servers.find((x) => x.name === name);
    const up = s?.state === "running" || s?.state === "starting";
    const where = s?.url?.replace(/^https?:\/\//, "");
    const state = s?.state === "running" ? t("preview.running") : s?.state === "starting" ? t("preview.starting") : s?.state === "failed" ? t("preview.failed") : t("preview.stopped");
    const group = `${name} · ${state}${up && where ? ` · ${where}` : ""}`;
    if (up) {
      if (s?.url) items.push({ id: `open:${name}`, group, icon: "globe", label: t("common.open"), disabled: s.state !== "running", onSelect: () => app.openInBrowser(sessionId, s.url!) });
      items.push({ id: `stop:${name}`, group, icon: "stop", label: t("preview.stop"), onSelect: () => void app.devServers.stop(sessionId, name).catch(fail) });
    } else {
      items.push({
        id: `start:${name}`,
        group,
        icon: "play",
        label: t("preview.start"),
        title: s?.error ?? undefined,
        onSelect: () => void app.devServers.start(sessionId, name).catch(fail),
      });
    }
  }
  if (!names.length) items.push({ id: "none", label: t("preview.none"), hint: t("preview.noneHint"), disabled: true, onSelect: () => {} });
  if (list.config_error) items.push({ id: "error", label: t("preview.configError"), hint: list.config_error, disabled: true, danger: true, onSelect: () => {} });
  items.push({
    id: "stop-all",
    icon: "stop",
    label: t("preview.stopAll"),
    danger: true,
    separatorBefore: names.length > 0,
    disabled: !live.length,
    onSelect: () => void app.devServers.stop(sessionId).catch(fail),
  });
  return (
    <Menu
      label={t("preview.menu")}
      align="right"
      trigger={(p) => (
        <Button {...p} size="sm" variant="ghost" icon="play" onClick={() => void app.devServers.refresh(sessionId)}>
          {t("preview.menu")}
          {live.length > 0 && <span className="devservers__count">{live.length}</span>}
        </Button>
      )}
      items={items}
    />
  );
}
