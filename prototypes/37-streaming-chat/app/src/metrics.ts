// Frame / long-task / event-loop-lag / latency recorders, bucketed per bench phase.
export type Phase = string;

export const pct = (xs: number[], p: number) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return +s[Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1))].toFixed(2);
};
const max = (xs: number[]) => (xs.length ? +Math.max(...xs).toFixed(2) : null);

class Bucket {
  frames: number[] = [];
  longtasks: number[] = [];
  lags: number[] = [];
  latency: number[] = [];
  ipcLatency: number[] = [];
  tokens = 0;
  t0 = performance.now();
  t1 = 0;
  summary() {
    const dur = (this.t1 || performance.now()) - this.t0;
    return {
      duration_ms: Math.round(dur),
      frames: { count: this.frames.length, p50: pct(this.frames, 50), p95: pct(this.frames, 95), p99: pct(this.frames, 99), max: max(this.frames), janky_over_25ms: this.frames.filter((f) => f > 25).length },
      longtasks: { supported: longtaskSupported, count: this.longtasks.length, max: max(this.longtasks) },
      event_loop_lag_ms: { p95: pct(this.lags, 95), max: max(this.lags) },
      emit_to_paint_ms: { n: this.latency.length, p50: pct(this.latency, 50), p95: pct(this.latency, 95), p99: pct(this.latency, 99), max: max(this.latency) },
      emit_to_js_ms: { p50: pct(this.ipcLatency, 50), p95: pct(this.ipcLatency, 95) },
      tokens: this.tokens,
      rendered_tok_per_s: this.tokens ? +((this.tokens / dur) * 1000).toFixed(1) : null,
    };
  }
}

let longtaskSupported = false;
export const rec = {
  buckets: new Map<Phase, Bucket>(),
  cur: null as Bucket | null,
  start(phase: Phase) {
    const b = new Bucket();
    this.buckets.set(phase, b);
    this.cur = b;
    return b;
  },
  stop() {
    if (this.cur) this.cur.t1 = performance.now();
    this.cur = null;
  },
};

let last = 0;
export function onFrame(ts: number) {
  if (last && rec.cur) rec.cur.frames.push(ts - last);
  last = ts;
}

try {
  new PerformanceObserver((l) => {
    for (const e of l.getEntries()) rec.cur?.longtasks.push(e.duration);
  }).observe({ type: "longtask", buffered: false });
  longtaskSupported = PerformanceObserver.supportedEntryTypes?.includes("longtask") ?? false;
} catch {
  longtaskSupported = false;
}

// Event-loop lag: how late a 20 ms interval fires (proxy for input responsiveness).
let expected = performance.now() + 20;
setInterval(() => {
  const now = performance.now();
  rec.cur?.lags.push(Math.max(0, now - expected));
  expected = now + 20;
}, 20);
