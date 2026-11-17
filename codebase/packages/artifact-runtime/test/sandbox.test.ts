// The artifact sandbox in a real browser engine (05 §8, D28): headless Chrome/Edge
// (Chromium = WebView2's engine) loads a parent page with `<iframe sandbox="allow-scripts">`
// whose document carries the desktop shell's CSP (read from `artifact.rs`, with the
// `genie-artifact:` source mapped to this test server's origin) and the shell's bridge
// script. It checks that
//   - a React artifact compiles in the sandbox with esbuild-wasm and renders with bundled
//     libraries (lodash, lucide-react) and Tailwind;
//   - an unknown import is reported back as `runtime_error` (the "Fix with Genie" path);
//   - the hostile artifact: IPC, fetch, XHR, WebSocket, EventSource, sendBeacon, top
//     navigation, parent DOM, storage/cookies, form posts, window.open, nested frames,
//     inline/data:/blob: scripts, workers, cross-origin images and imports all fail, and
//     the server never sees any of those requests.
// Skipped when no Chromium-based browser is installed or the runtime isn't built. WKWebView
// and WebKitGTK runs need the real app (see the README gaps).
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "..", "dist");
const artifactRs = join(here, "..", "..", "..", "apps", "desktop", "src-tauri", "src", "artifact.rs");

const CANDIDATES = [
  process.env.GENIE_TEST_CHROME ?? "",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
  "/usr/bin/microsoft-edge",
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
].filter(Boolean);
const chrome = CANDIDATES.find((p) => existsSync(p));
const enabled = !!chrome && existsSync(join(dist, "runtime.js")) && existsSync(artifactRs);

/** `CSP` and `BRIDGE_JS` exactly as the desktop shell serves them. */
function shellConstants() {
  const rs = readFileSync(artifactRs, "utf8");
  const csp = /pub const CSP: &str = "([\s\S]*?)";/.exec(rs)![1].replace(/\\\n\s*/g, "");
  const bridge = /pub const BRIDGE_JS: &str = r#"([\s\S]*?)"#;/.exec(rs)![1];
  return { csp, bridge };
}

const MIME: Record<string, string> = { ".js": "text/javascript", ".css": "text/css", ".json": "application/json", ".woff2": "font/woff2", ".woff": "font/woff", ".ttf": "font/ttf" };

const PARENT = `<!doctype html><html><body><pre id="out">pending</pre>
<iframe id="f" sandbox="allow-scripts" src="/art/ID/"></iframe>
<script>
const msgs = [];
const f = document.getElementById("f");
window.addEventListener("message", (e) => {
  if (e.source !== f.contentWindow) return;
  msgs.push(e.data);
  document.getElementById("out").textContent = JSON.stringify(msgs);
  if (e.data && (e.data.type === "probe" || e.data.type === "runtime_error")) report();
});
// The app-side page (not sandboxed) hands the messages to the test server.
const report = () => fetch("/report/ID", { method: "POST", body: JSON.stringify(msgs) });
setTimeout(report, 45000);
</script></body></html>`;

const ARTIFACTS: Record<string, string> = {
  ok: `
import { useEffect, useState } from "react";
import { Camera } from "lucide-react";
import _ from "lodash";
export default function App() {
  const [n] = useState(3);
  useEffect(() => {
    setTimeout(() => {
      const root = document.getElementById("root");
      const svg = root.querySelector("svg");
      const bold = getComputedStyle(root.querySelector("p")).fontWeight;
      parent.postMessage({ type: "probe", text: root.textContent, svg: !!svg, bold }, "*");
    }, 300);
  }, []);
  return <div><p className="font-bold">{_.repeat("ab", n)}</p><Camera /></div>;
}`,
  unknown: `import axios from "axios";\nexport default function App() { axios.get("x"); return null; }`,
  crash: `export default function App() { throw new Error("boom in render"); }`,
  hostile: `
import { useEffect } from "react";
const ORIGIN = "__ORIGIN__";
const OTHER = "__OTHER__";
const results = {};
async function attempt(name, f, ms = 1500) {
  try {
    const r = await Promise.race([Promise.resolve().then(f), new Promise((res) => setTimeout(() => res("__timeout__"), ms))]);
    // "__server__": the browser API reports success either way (sendBeacon queues; a frame
    // blocked by frame-src still fires load on an error page): the server hit list decides.
    results[name] = r === "__blocked__" || r === "__timeout__" ? "blocked" : r === "__server__" ? "server" : "ALLOWED:" + String(r).slice(0, 80);
  } catch (e) {
    results[name] = "blocked";
  }
}
const loads = (el) => new Promise((res) => { el.onload = () => res("loaded"); el.onerror = () => res("__blocked__"); });
async function run() {
  await attempt("tauri_internals", () => (typeof window.__TAURI_INTERNALS__ === "undefined" && typeof window.__TAURI__ === "undefined" && typeof window.ipc === "undefined" ? "__blocked__" : "present"));
  await attempt("ipc_fetch", () => fetch("ipc://localhost/host_request", { method: "POST", body: "{}" }).then((r) => r.status));
  await attempt("ipc_localhost", () => fetch("http://ipc.localhost/host_request", { method: "POST" }).then((r) => r.status));
  await attempt("fetch", () => fetch(ORIGIN + "/hit/fetch").then((r) => r.status));
  await attempt("fetch_other", () => fetch(OTHER + "/hit/fetch-other").then((r) => r.status));
  await attempt("xhr", () => new Promise((res, rej) => { const x = new XMLHttpRequest(); x.open("GET", ORIGIN + "/hit/xhr"); x.onload = () => res(x.status); x.onerror = () => rej(new Error("x")); x.send(); }));
  await attempt("websocket", () => new Promise((res, rej) => { const w = new WebSocket(ORIGIN.replace("http", "ws") + "/hit/ws"); w.onopen = () => res("open"); w.onerror = () => rej(new Error("ws")); }));
  await attempt("eventsource", () => new Promise((res, rej) => { const s = new EventSource(ORIGIN + "/hit/sse"); s.onopen = () => res("open"); s.onerror = () => { s.close(); rej(new Error("sse")); }; }));
  await attempt("beacon", () => (navigator.sendBeacon(ORIGIN + "/hit/beacon", "x") ? "__server__" : "__blocked__"));
  await attempt("parent_dom", () => parent.document.body.innerHTML);
  await attempt("top_dom", () => top.document.title);
  await attempt("top_navigation", () => { top.location.href = ORIGIN + "/hit/navigate"; return "__timeout__"; });
  await attempt("local_storage", () => { localStorage.setItem("a", "b"); return localStorage.getItem("a"); });
  await attempt("session_storage", () => { sessionStorage.setItem("a", "b"); return "set"; });
  await attempt("cookie", () => { document.cookie = "a=b"; return document.cookie || "__blocked__"; });
  await attempt("indexeddb", () => new Promise((res, rej) => { const r = indexedDB.open("x"); r.onsuccess = () => res("open"); r.onerror = () => rej(new Error("idb")); }));
  await attempt("window_open", () => { const w = window.open(ORIGIN + "/hit/popup"); return w ? "opened" : "__blocked__"; });
  await attempt("alert_shimmed", () => { const t = Date.now(); alert("x"); confirm("y"); prompt("z"); return Date.now() - t < 1000 ? "__blocked__" : "blocked the thread"; });
  await attempt("form_post", () => { const f = document.createElement("form"); f.method = "POST"; f.action = ORIGIN + "/hit/form"; document.body.appendChild(f); f.submit(); return "__timeout__"; });
  await attempt("nested_frame", () => { const i = document.createElement("iframe"); i.src = ORIGIN + "/hit/frame"; const p = loads(i); document.body.appendChild(i); return p.then(() => "__server__"); });
  await attempt("inline_script", () => { const s = document.createElement("script"); s.textContent = "window.__pwn_inline = 1"; document.body.appendChild(s); return window.__pwn_inline ? "ran" : "__blocked__"; });
  await attempt("document_write", () => { document.write("<script>window.__pwn_write = 1<\\/script>"); return window.__pwn_write ? "ran" : "__blocked__"; });
  await attempt("data_script", () => { const s = document.createElement("script"); s.src = "data:text/javascript,window.__pwn_data=1"; const p = loads(s); document.body.appendChild(s); return p; });
  await attempt("blob_script", () => { const s = document.createElement("script"); s.src = URL.createObjectURL(new Blob(["window.__pwn_blob=1"], { type: "text/javascript" })); const p = loads(s); document.body.appendChild(s); return p; });
  await attempt("worker", () => new Promise((res, rej) => { const w = new Worker(URL.createObjectURL(new Blob(["postMessage(1)"], { type: "text/javascript" }))); w.onmessage = () => res("ran"); w.onerror = () => rej(new Error("w")); }));
  await attempt("other_origin_script", () => { const s = document.createElement("script"); s.src = OTHER + "/hit/script.js"; const p = loads(s); document.body.appendChild(s); return p; });
  await attempt("other_origin_image", () => { const i = new Image(); const p = loads(i); i.src = OTHER + "/hit/img.png"; return p; });
  await attempt("dynamic_import", () => import(/* @vite-ignore */ OTHER + "/hit/mod.js").then(() => "imported"));
  await attempt("meta_csp_loosen", () => { const m = document.createElement("meta"); m.httpEquiv = "Content-Security-Policy"; m.content = "connect-src *"; document.head.appendChild(m); return fetch(ORIGIN + "/hit/after-meta").then((r) => r.status); });
  await attempt("css_exfil", () => { const l = document.createElement("link"); l.rel = "stylesheet"; l.href = OTHER + "/hit/style.css"; const p = loads(l); document.head.appendChild(l); return p; });
  parent.postMessage({ type: "probe", results }, "*");
}
export default function Hostile() {
  useEffect(() => { run(); }, []);
  return <p>hostile</p>;
}`,
};

let server: Server;
let origin = "";
let other = "";
const hits: string[] = [];
const reports = new Map<string, (msgs: unknown[]) => void>();

beforeAll(async () => {
  if (!enabled) return;
  const { csp, bridge } = shellConstants();
  server = createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://x");
    const path = url.pathname;
    const send = (status: number, type: string, body: string | Buffer, extra: Record<string, string> = {}) => {
      res.writeHead(status, { "Content-Type": type, "Cache-Control": "no-store", ...extra });
      res.end(body);
    };
    const rep = /^\/report\/(\w+)$/.exec(path);
    if (rep && req.method === "POST") {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        reports.get(rep[1])?.(JSON.parse(body || "[]") as unknown[]);
        send(204, "text/plain", "");
      });
      return;
    }
    if (path.startsWith("/hit/")) {
      hits.push(path);
      return send(200, path.endsWith(".js") ? "text/javascript" : "text/plain", "window.__pwn_remote=1", { "Access-Control-Allow-Origin": "*" });
    }
    const artCsp = csp.replaceAll("http://genie-artifact.localhost", origin).replaceAll("genie-artifact:", origin);
    const page = (body: string, type = "text/html") => send(200, type, body, { "Content-Security-Policy": artCsp, "X-Content-Type-Options": "nosniff" });
    const m = /^\/parent\/(\w+)$/.exec(path);
    if (m) return send(200, "text/html", PARENT.replaceAll("ID", m[1]));
    const a = /^\/art\/(\w+)\/(.*)$/.exec(path);
    if (a) {
      const src = ARTIFACTS[a[1]]?.replaceAll("__ORIGIN__", origin).replaceAll("__OTHER__", other);
      if (src == null) return send(404, "text/plain", "no");
      if (a[2] === "") return page(`<!doctype html><html><head><meta charset="utf-8"><title>t</title><script src="/_genie/bridge.js"></script></head><body><div id="root"></div><script src="source.js"></script><script src="/_genie/runtime/runtime.js"></script></body></html>`);
      if (a[2] === "source.js") return page(`globalThis.__GENIE_SOURCE__ = ${JSON.stringify(src)};`, "text/javascript");
      return send(404, "text/plain", "no");
    }
    if (path === "/_genie/bridge.js") return page(bridge, "text/javascript");
    if (path.startsWith("/_genie/runtime/")) {
      const rel = normalize(path.slice("/_genie/runtime/".length));
      if (rel.startsWith("..")) return send(404, "text/plain", "no");
      const file = join(dist, rel);
      if (!existsSync(file)) return send(404, "text/plain", "no");
      return page(readFileSync(file) as unknown as string, MIME[extname(file)] ?? "application/octet-stream");
    }
    if (path !== "/favicon.ico") hits.push(path); // the parent tab's own favicon request
    send(404, "text/plain", "no");
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", () => r()));
  const port = (server.address() as AddressInfo).port;
  origin = `http://127.0.0.1:${port}`;
  other = `http://localhost:${port}`; // a different origin served by the same process
});

afterAll(() => {
  server?.close();
});

/** Open the parent page in headless Chromium and wait for its report (or 60 s). */
async function run(id: string): Promise<unknown[]> {
  const profile = mkdtempSync(join(tmpdir(), "genie-sandbox-"));
  const child = spawn(chrome!, ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--disable-extensions", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "--enable-logging=stderr", `${origin}/parent/${id}`], { stdio: ["ignore", "ignore", "pipe"] });
  let log = "";
  child.stderr?.on("data", (c) => (log += String(c)));
  try {
    return await new Promise<unknown[]>((resolve) => {
      const timer = setTimeout(() => {
        console.error(`no report from ${id}; browser log:\n${log.split("\n").filter((l) => /CONSOLE|Refused|ERROR:(?!.*crashpad)/i.test(l)).join("\n").slice(-4000)}`);
        resolve([]);
      }, 60_000);
      reports.set(id, (msgs) => {
        clearTimeout(timer);
        resolve(msgs);
      });
    });
  } finally {
    reports.delete(id);
    child.kill();
    await new Promise((r) => setTimeout(r, 300));
    rmSync(profile, { recursive: true, force: true });
  }
}

const probe = (msgs: unknown[]) => msgs.find((m) => (m as { type?: string }).type === "probe") as Record<string, any> | undefined;

describe.skipIf(!enabled)("artifact sandbox in Chromium", () => {
  it("compiles and renders a React artifact with bundled libraries", async () => {
    const msgs = await run("ok");
    expect(msgs.some((m) => (m as { type: string }).type === "ready")).toBe(true);
    const p = probe(msgs);
    expect(p, JSON.stringify(msgs)).toBeDefined();
    expect(p!.text).toBe("ababab");
    expect(p!.svg).toBe(true);
    expect(p!.bold).toBe("700"); // Tailwind class generated inside the frame
    expect(msgs.some((m) => (m as { type: string }).type === "runtime_error")).toBe(false);
  }, 120_000);

  it("reports unknown imports and render crashes as runtime_error", async () => {
    const unknown = (await run("unknown")).find((m) => (m as { type: string }).type === "runtime_error") as { message: string } | undefined;
    expect(unknown?.message).toMatch(/Unknown import "axios"/);
    const crash = (await run("crash")).find((m) => (m as { type: string }).type === "runtime_error") as { message: string } | undefined;
    expect(crash?.message).toMatch(/boom in render/);
  }, 120_000);

  it("blocks every escape attempt of a hostile artifact", async () => {
    hits.length = 0;
    const msgs = await run("hostile");
    const p = probe(msgs);
    expect(p, JSON.stringify(msgs)).toBeDefined();
    const allowed = Object.entries(p!.results as Record<string, string>).filter(([, v]) => v !== "blocked" && v !== "server");
    expect(allowed).toEqual([]);
    // Give late requests (beacon, frames) a moment to show up.
    await new Promise((r) => setTimeout(r, 500));
    expect(Object.keys(p!.results).length).toBeGreaterThanOrEqual(30);
    expect(hits).toEqual([]);
  }, 120_000);
});
