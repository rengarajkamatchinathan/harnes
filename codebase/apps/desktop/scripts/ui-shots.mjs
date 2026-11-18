#!/usr/bin/env node
// Screenshot harness for the desktop UI (mock platform in plain Chrome, driven over CDP).
//
//   pnpm dev                       # in another terminal (http://localhost:5173)
//   pnpm ui:shots [outDir] [--widths=1440x900,1000x760,760x760] [--theme=dark|light|both]
//
// Walks the main states (empty chat, a streamed reply, model menu, settings + dropdown,
// collapsed sidebar, drawer, Code mode + right panel) at each width and writes PNGs, so the
// UI can be compared side by side with the reference screenshots. No dependencies: Node ≥ 22
// (global WebSocket/fetch) and a local Chrome/Edge (set CHROME to override the path).
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const args = process.argv.slice(2);
const flag = (name, dflt) => args.find((a) => a.startsWith(`--${name}=`))?.split("=")[1] ?? dflt;
const out = resolve(args.find((a) => !a.startsWith("--")) ?? ".ui-shots");
const widths = flag("widths", "1440x900,1000x760,760x760").split(",").map((w) => w.split("x").map(Number));
const themes = flag("theme", "dark") === "both" ? ["dark", "light"] : [flag("theme", "dark")];
const url = flag("url", "http://localhost:5173/");
const only = flag("only", "");
const CANDIDATES = [
  process.env.CHROME,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
].filter(Boolean);
const chrome = CANDIDATES.find((p) => existsSync(p));
if (!chrome) throw new Error("No Chrome/Edge found; set CHROME=/path/to/chrome");
mkdirSync(out, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9300 + Math.floor(Math.random() * 500);
const profile = mkdtempSync(join(tmpdir(), "genie-shots-"));
const proc = spawn(chrome, ["--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "--hide-scrollbars", "about:blank"], { stdio: "ignore" });

let ws;
for (let i = 0; i < 60 && !ws; i++) {
  try {
    const page = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === "page");
    if (page) {
      const sock = new WebSocket(page.webSocketDebuggerUrl);
      await new Promise((ok, fail) => (sock.addEventListener("open", ok), sock.addEventListener("error", fail)));
      ws = sock;
      break;
    }
  } catch {}
  await sleep(150);
}
if (!ws) throw new Error("Could not connect to Chrome over CDP");
let seq = 0;
const pending = new Map();
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) (pending.get(m.id)(m), pending.delete(m.id));
});
const send = (method, params = {}) => new Promise((r) => (pending.set(++seq, r), ws.send(JSON.stringify({ id: seq, method, params }))));
const ev = async (expression) => (await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;

/** Centre of the first element matching `sel` whose text or aria-label contains `text`. */
const find = (sel, text = "") =>
  ev(`(() => { const els = [...document.querySelectorAll(${JSON.stringify(sel)})].filter(e => {
      if (!e.getClientRects().length || e.closest('[inert]')) return false;
      const r = e.getBoundingClientRect(); return r.right > 0 && r.left < innerWidth && r.bottom > 0 && r.top < innerHeight; });
    const el = els.find(e => !${JSON.stringify(text)} || (e.getAttribute('aria-label') || '').includes(${JSON.stringify(text)}) || (e.textContent || '').trim().includes(${JSON.stringify(text)}));
    if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
const mouse = async (type, x, y) => send("Input.dispatchMouseEvent", { type, x, y, button: "left", buttons: type === "mouseReleased" ? 0 : 1, clickCount: 1, pointerType: "mouse" });
async function click(sel, text) {
  const p = await find(sel, text);
  if (!p) return console.warn(`  ! not found: ${sel} ${text ?? ""}`), false;
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y });
  await mouse("mousePressed", p.x, p.y);
  await mouse("mouseReleased", p.x, p.y);
  return true;
}
const key = async (k, code = k, modifiers = 0) => {
  const vk = { Escape: 27, Enter: 13, Tab: 9, Backspace: 8, ArrowDown: 40, ArrowUp: 38 }[k] ?? 0;
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: k, code, modifiers, windowsVirtualKeyCode: vk });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: k, code, modifiers });
};
const type = (text) => send("Input.insertText", { text });

for (const theme of themes)
  for (const [w, h] of widths) {
    const tag = `${theme}-${w}`;
    const shot = async (name) => {
      if (only && !name.includes(only)) return;
      await sleep(350); // let enter animations settle
      const r = await send("Page.captureScreenshot", { format: "png" });
      const file = join(out, `${tag}-${name}.png`);
      writeFileSync(file, Buffer.from(r.result.data, "base64"));
      console.log(`  ${file}`);
    };
    console.log(`${theme} ${w}×${h}`);
    await send("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 1, mobile: false });
    await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: theme }] });
    await send("Page.navigate", { url });
    await sleep(2200);
    await shot("01-home");

    // The / menu (D58): names, the selected row's tooltip, then a picked command's token + hint.
    await click("textarea");
    await type("/");
    await sleep(300);
    await shot("01b-slash-menu");
    await key("ArrowDown");
    await key("ArrowDown");
    await shot("01c-slash-tip");
    await type("rev");
    await sleep(150);
    await key("Tab");
    await shot("01d-slash-token");
    for (let i = 0; i < 12; i++) await key("Backspace");

    await click("textarea");
    await type("Show me a markdown table and a code sample");
    await key("Enter");
    await sleep(600);
    await shot("02-streaming");
    await sleep(3500);
    await shot("03-reply");
    await click("textarea");
    await type("/");
    await sleep(300);
    await shot("03b-slash-reply");
    await key("Escape");
    await key("Backspace");

    // Right panel while a chat is open (at mid widths the sidebar turns into a drawer).
    if (await click(".chat__header .header-icons button")) {
      await sleep(400);
      await shot("04a-panel");
      await click(".panel__head button[aria-label='Hide panel'], .panel__head button:last-of-type");
      await sleep(300);
    }

    // Tooltip on the sidebar toggle.
    {
      const p = await find("button[aria-label='Toggle sidebar']");
      if (p) {
        await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y });
        await sleep(800);
        await shot("04b-tooltip");
        await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: w / 2, y: h / 2 });
      }
    }

    if (await click(".modelpick button")) {
      await shot("04-model-menu");
      await key("Escape");
    }

    if (await click(".sidebar__row .sidebar__more, .sidebar__row button[aria-label='Actions']")) {
      await shot("05-row-menu");
      await key("Escape");
    }

    // Sidebar: collapse inline (wide) or open the drawer (overlay layouts).
    await click("button[aria-label='Toggle sidebar']");
    await sleep(400);
    await shot("06-sidebar-toggled");
    await click("button[aria-label='Toggle sidebar']");
    await sleep(400);

    await click(".profile");
    await shot("07-profile-menu");
    await key("Escape");

    await click(".sidebar__footer button[aria-label='Settings']");
    await shot("08-settings");
    if (await click(".settings button[role='combobox']")) {
      await shot("09-settings-select");
      await key("Escape");
    }
    await key("Escape");
    await click(".settings__head button");
    await sleep(300);

    await click(".modeswitch__btn", "Code");
    await sleep(500);
    await shot("10-code-home");

  }

ws.close();
proc.kill();
await sleep(200);
rmSync(profile, { recursive: true, force: true });
process.exit(0);
