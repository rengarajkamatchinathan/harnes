// Non-React streaming engine: IPC Channel in, one rAF-batched React commit out per frame.
import { Channel, invoke } from "@tauri-apps/api/core";
import { flushSync } from "react-dom";
import { BlockSplitter } from "./markdown";
import { onFrame, rec } from "./metrics";

export type Msg = { id: number; role: "user" | "assistant"; text: string };
export type Status = { state: string; detail: string; pid: number; reconnect_ms?: number | null; detect_ms?: number | null };
export type Info = { bench: boolean; start_wall_ms: number; coalesce_ms: number; os: string; arch: string };

export const engine = {
  messages: [] as Msg[],
  splitter: null as BlockSplitter | null,
  pendingText: "",
  pendingEmits: [] as number[],
  toPaint: [] as number[],
  ended: false,
  dirty: false,
  stick: true,
  nextId: 1,
  status: null as Status | null,
  statusLog: [] as (Status & { t: number })[],
  info: null as Info | null,
  firstPaintWall: 0,
  scrollEl: null as HTMLDivElement | null,
  scrollDriver: null as null | ((ts: number) => void),
  onEnded: null as null | (() => void),
  onFirstDelta: null as null | (() => void),
  onStatus: new Set<(s: Status) => void>(),
  rerender: () => {},

  reset(prompt: string | null) {
    this.messages = prompt ? [{ id: this.nextId++, role: "user", text: prompt }] : [];
    this.splitter = new BlockSplitter();
    this.pendingText = "";
    this.pendingEmits = [];
    this.toPaint = [];
    this.ended = false;
    this.stick = true;
    this.dirty = true;
  },
};

export async function connect(): Promise<Info> {
  const ch = new Channel<any>();
  ch.onmessage = (ev) => {
    switch (ev.kind) {
      case "deltas": {
        if (engine.onFirstDelta) {
          const f = engine.onFirstDelta;
          engine.onFirstDelta = null;
          f();
        }
        engine.pendingText += ev.text;
        for (const e of ev.emits_ms) engine.pendingEmits.push(e);
        if (rec.cur) {
          const now = Date.now();
          for (const e of ev.emits_ms) rec.cur.ipcLatency.push(now - e);
        }
        break;
      }
      case "ended":
        engine.ended = true;
        break;
      case "history": {
        const hist: Msg[] = ev.messages.map((m: any) => ({ id: engine.nextId++, role: m.role, text: m.text }));
        engine.messages = [...hist, ...engine.messages];
        invoke("log", { msg: `history: ${hist.length} msgs` });
        engine.dirty = true;
        break;
      }
      case "status":
        engine.status = ev;
        engine.statusLog.push({ ...ev, t: Date.now() });
        engine.onStatus.forEach((f) => f(ev));
        engine.dirty = true;
        break;
    }
  };
  const info = await invoke<Info>("subscribe", { onEvent: ch });
  engine.info = info;
  return info;
}

function loop(ts: number) {
  onFrame(ts);
  engine.scrollDriver?.(ts);
  const e = engine;
  // deltas committed last frame have been painted by the time this callback runs
  if (e.toPaint.length) {
    const now = Date.now();
    if (rec.cur) for (const t of e.toPaint) rec.cur.latency.push(now - t);
    e.toPaint = [];
  }
  let dirty = e.dirty;
  e.dirty = false;
  if (e.pendingText && e.splitter) {
    e.splitter.push(e.pendingText);
    if (rec.cur) rec.cur.tokens += e.pendingEmits.length;
    e.toPaint = e.pendingEmits;
    e.pendingText = "";
    e.pendingEmits = [];
    dirty = true;
  }
  let endedNow = false;
  if (e.ended) {
    e.ended = false;
    if (e.splitter) {
      e.splitter.finish();
      e.messages = [...e.messages, { id: e.nextId++, role: "assistant", text: e.splitter.text }];
      e.splitter = null;
    }
    dirty = true;
    endedNow = true;
  }
  if (dirty) {
    flushSync(() => e.rerender());
    if (e.stick && e.scrollEl) e.scrollEl.scrollTop = e.scrollEl.scrollHeight;
  }
  if (endedNow) invoke("log", { msg: "stream ended" });
  if (endedNow && e.onEnded) {
    const f = e.onEnded;
    e.onEnded = null;
    f();
  }
  requestAnimationFrame(loop);
}
export const startLoop = () => requestAnimationFrame(loop);
