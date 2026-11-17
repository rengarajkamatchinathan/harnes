// Prints a compact summary of a bench-results JSON: node summarize.mjs bench-results.json
import { readFileSync } from "node:fs";
const r = JSON.parse(readFileSync(process.argv[2] || "bench-results.json", "utf8"));
const p = r.phases || {};
const row = (k) => {
  const b = p[k];
  if (!b) return;
  console.log(
    `${k.padEnd(17)} frames p50/p95/p99/max ${b.frames.p50}/${b.frames.p95}/${b.frames.p99}/${b.frames.max} ms (n=${b.frames.count}, >25ms: ${b.frames.janky_over_25ms})` +
      ` | longtasks ${b.longtasks.supported ? `${b.longtasks.count} (max ${b.longtasks.max})` : "n/a"}` +
      ` | lag max ${b.event_loop_lag_ms.max}` +
      (b.emit_to_paint_ms.n ? ` | emit→paint p50/p95 ${b.emit_to_paint_ms.p50}/${b.emit_to_paint_ms.p95} ms | ${b.rendered_tok_per_s} tok/s` : "")
  );
};
console.log(`UA: ${r.user_agent}\nOS: ${r.info?.os}/${r.info?.arch}  coalesce: ${r.info?.coalesce_ms} ms  DPR: ${r.device_pixel_ratio}`);
if (r.error) console.log("ERROR:", r.error);
Object.keys(p).forEach(row);
console.log("cold start:", JSON.stringify(r.cold_start));
console.log("kill:", JSON.stringify(r.kill));
console.log("memory end:", JSON.stringify(r.memory_end));
console.log("sizes:", JSON.stringify({ exe_mb: r.rust?.exe_mb, daemon_mb: r.rust?.daemon_exe_mb }));
console.log("highlighters:", JSON.stringify(r.highlighters));
console.log("PASS:", JSON.stringify(r.pass, null, 1));
