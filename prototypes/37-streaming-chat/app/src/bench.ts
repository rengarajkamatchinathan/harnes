// --bench / GENIE_BENCH=1: run every scenario, the highlighter comparison and the kill test, then write results.
import { invoke } from "@tauri-apps/api/core";
import { engine } from "./engine";
import { hljs } from "./markdown";
import { pct, rec } from "./metrics";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const frames = (n: number) => new Promise<void>((r) => { const f = () => (--n <= 0 ? r() : requestAnimationFrame(f)); requestAnimationFrame(f); });
const log = (msg: string) => invoke("log", { msg }).catch(() => {});
const heapMb = () => { const m = (performance as any).memory; return m ? +(m.usedJSHeapSize / 1048576).toFixed(1) : null; };

function waitStatus(state: string, timeoutMs = 10000) {
  return new Promise<any>((res, rej) => {
    if (engine.status?.state === state) return res(engine.status);
    const t = setTimeout(() => (engine.onStatus.delete(f), rej(new Error(`timeout waiting for ${state}`))), timeoutMs);
    const f = (s: any) => { if (s.state === state) { clearTimeout(t); engine.onStatus.delete(f); res(s); } };
    engine.onStatus.add(f);
  });
}

/** Waits for a status (by log position, so events that arrive before we start waiting aren't missed). */
async function waitLog(from: number, state: string, timeoutMs: number) {
  const t0 = performance.now();
  for (;;) {
    const hit = engine.statusLog.slice(from).find((s) => s.state === state);
    if (hit) return hit;
    if (performance.now() - t0 > timeoutMs) throw new Error(`timeout waiting for ${state}`);
    await sleep(5);
  }
}

async function runScenario(sc: string) {
  engine.reset(sc === "long" ? null : `Run the ${sc} scenario`);
  await frames(10);
  const out: any = { heap_before_mb: heapMb() };
  const ended = new Promise<void>((r) => (engine.onEnded = r));
  const t0 = performance.now();
  if (sc === "long") {
    rec.start("long-history");
    engine.onFirstDelta = () => { rec.start("long-stream"); };
  } else rec.start(sc);
  await invoke("chat_start", { scenario: sc });
  await ended;
  rec.stop();
  out.wall_ms = Math.round(performance.now() - t0);
  await frames(30);
  out.heap_after_mb = heapMb();
  out.rss = await invoke("metrics");
  out.dom_nodes = document.getElementsByTagName("*").length;
  if (sc === "long") {
    out.messages = engine.messages.length;
    out.scroll = await scrollTest();
  }
  return out;
}

/** Programmatic fling: scroll the transcript up 400 px per frame for 300 frames, then back down. */
async function scrollTest() {
  const el = engine.scrollEl!;
  engine.stick = false;
  await frames(10);
  const startTop = el.scrollTop;
  rec.start("long-scroll");
  let n = 0;
  await new Promise<void>((r) => {
    engine.scrollDriver = () => {
      el.scrollTop = Math.max(0, el.scrollTop - 400);
      if (++n >= 300 || el.scrollTop === 0) { engine.scrollDriver = null; r(); }
    };
  });
  rec.stop();
  const travelled = startTop - el.scrollTop;
  await frames(10);
  rec.start("long-scroll-down");
  n = 0;
  await new Promise<void>((r) => {
    engine.scrollDriver = () => {
      el.scrollTop = el.scrollTop + 400;
      if (++n >= 300) { engine.scrollDriver = null; r(); }
    };
  });
  rec.stop();
  return { px_up: Math.round(travelled), frames_each_way: n };
}

async function highlighterBench(text: string) {
  const blocks = [...text.matchAll(/```(\w+)\n([\s\S]*?)```/g)].filter((m) => m[1] !== "mermaid").map((m) => ({ lang: m[1], code: m[2] }));
  const time = (fn: () => void, reps: number) => { const t = performance.now(); for (let i = 0; i < reps; i++) fn(); return (performance.now() - t) / reps; };
  // "tail" simulation: re-highlight every 16-char prefix, as the streaming tail does each frame
  const tailCalls = (hl: (code: string, lang: string) => void) => {
    const per: number[] = [];
    for (const b of blocks) for (let n = 16; n < b.code.length + 16; n += 16) { const t = performance.now(); hl(b.code.slice(0, n), b.lang); per.push(performance.now() - t); }
    return { calls: per.length, p50_ms: pct(per, 50), p95_ms: pct(per, 95), max_ms: pct(per, 100) };
  };
  const hljsFn = (code: string, lang: string) => { hljs.highlight(code, { language: lang }).value; };
  const res: any = { blocks: blocks.length, langs: blocks.map((b) => b.lang) };
  res.hljs = { full_all_blocks_ms: +time(() => blocks.forEach((b) => hljsFn(b.code, b.lang)), 20).toFixed(3), tail: tailCalls(hljsFn) };
  try {
    const t0 = performance.now();
    const { createHighlighterCore } = await import("shiki/core");
    const { createJavaScriptRegexEngine } = await import("shiki/engine/javascript");
    const hl = await createHighlighterCore({
      themes: [import("shiki/themes/github-dark.mjs")],
      langs: [import("shiki/langs/rust.mjs"), import("shiki/langs/typescript.mjs"), import("shiki/langs/python.mjs"), import("shiki/langs/go.mjs"), import("shiki/langs/sql.mjs"), import("shiki/langs/bash.mjs")],
      engine: createJavaScriptRegexEngine(),
    });
    const init = performance.now() - t0;
    const shikiFn = (code: string, lang: string) => { hl.codeToHtml(code, { lang, theme: "github-dark" }); };
    shikiFn(blocks[0].code, blocks[0].lang); // warm
    res.shiki_js_engine = { init_ms: +init.toFixed(1), full_all_blocks_ms: +time(() => blocks.forEach((b) => shikiFn(b.code, b.lang)), 20).toFixed(3), tail: tailCalls(shikiFn) };
  } catch (e) {
    res.shiki_js_engine = { error: String(e) };
  }
  return res;
}

async function killTest(times: number) {
  const runs: any[] = [];
  rec.start("kill");
  for (let i = 0; i < times; i++) {
    await sleep(1000);
    await waitStatus("connected");
    const from = engine.statusLog.length;
    const k: any = await invoke("kill_daemon");
    const lost = await waitLog(from, "reconnecting", 5000);
    const back = await waitLog(from, "connected", 10000);
    runs.push({ killed_pid: k.pid, new_pid: back.pid, detect_ms: lost.detect_ms != null ? +lost.detect_ms.toFixed(1) : null, reconnect_ms: back.reconnect_ms != null ? +back.reconnect_ms.toFixed(1) : null });
    log(`kill ${i + 1}: ${JSON.stringify(runs[runs.length - 1])}`);
  }
  rec.stop();
  // prove the new daemon actually works: a short chat on the respawned daemon
  engine.reset("Chat after reconnect");
  const ended = new Promise<string>((r) => { engine.onEnded = () => r("ok: stream completed"); setTimeout(() => r("timeout"), 15000); });
  const r = await invoke("chat_start", { scenario: "burst" }).then(() => ended, (e) => String(e));
  const recon = runs.map((r) => r.reconnect_ms ?? Infinity);
  return { runs, max_reconnect_ms: Math.max(...recon), chat_after_reconnect: r };
}

export async function runBench(setPhase: (s: string) => void) {
  const info = engine.info!;
  const out: any = { started_at: new Date().toISOString(), info, user_agent: navigator.userAgent, device_pixel_ratio: devicePixelRatio, window: [innerWidth, innerHeight], scenarios: {} };
  try {
    setPhase("bench: waiting for daemon");
    const connected = await waitStatus("connected", 15000);
    const fcp = performance.getEntriesByName("first-contentful-paint")[0]?.startTime;
    out.cold_start = {
      process_start_to_first_paint_ms: Math.round(engine.firstPaintWall - info.start_wall_ms),
      process_start_to_webview_nav_ms: Math.round(performance.timeOrigin - info.start_wall_ms),
      fcp_after_nav_ms: fcp != null ? Math.round(fcp) : null,
      process_start_to_daemon_connected_ms: Math.round(engine.statusLog.find((s) => s.state === "connected")!.t - info.start_wall_ms),
      daemon_was_spawned: engine.statusLog.some((s) => s.state === "spawning"),
      connected_pid: connected.pid,
    };
    setPhase("bench: idle baseline");
    rec.start("idle");
    await sleep(2000);
    rec.stop();
    let corpusText = "";
    const only = (info as any).scenarios as string | undefined;
    for (const sc of (only ? only.split(",") : ["normal", "burst", "long"])) {
      setPhase(`bench: ${sc}`);
      log(`scenario ${sc}`);
      out.scenarios[sc] = await runScenario(sc);
      if (sc === "normal") corpusText = engine.messages[engine.messages.length - 1].text;
    }
    setPhase("bench: highlighters");
    out.highlighters = await highlighterBench(corpusText);
    setPhase("bench: kill/reconnect");
    out.kill = await killTest(3);
    out.memory_end = { js_heap_mb: heapMb(), rss: await invoke("metrics") };
  } catch (e) {
    out.error = String(e);
    log("bench error: " + e);
  }
  out.phases = Object.fromEntries([...rec.buckets].map(([k, b]) => [k, b.summary()]));
  out.pass = evaluate(out);
  setPhase("bench: writing results");
  const path = await invoke<string>("write_results", { results: out });
  setPhase(`bench done → ${path}`);
}

function evaluate(out: any) {
  const p = out.phases;
  const lt = (b: any, ms: number) => (b?.longtasks?.supported ? (b.longtasks.max ?? 0) <= ms : null);
  return {
    normal_p95_frame_le_16_7: p.normal ? p.normal.frames.p95 <= 16.7 : null,
    normal_no_longtask_gt_50: lt(p.normal, 50),
    normal_emit_to_paint_p95_le_50: p.normal ? p.normal.emit_to_paint_ms.p95 <= 50 : null,
    burst_no_longtask_gt_100: lt(p.burst, 100),
    burst_event_loop_lag_max_lt_100: p.burst ? p.burst.event_loop_lag_ms.max < 100 : null,
    long_scroll_p95_frame_le_16_7: p["long-scroll"] ? p["long-scroll"].frames.p95 <= 16.7 : null,
    kill_reconnect_le_2000: out.kill ? out.kill.max_reconnect_ms <= 2000 : null,
    note: "frame p95 compares to a strict 16.7 ms; see phases.idle.frames for the vsync baseline of this display",
  };
}
