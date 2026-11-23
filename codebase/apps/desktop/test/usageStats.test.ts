import { describe, expect, it } from "vitest";
import { applyEvent, emptyModel } from "../src/state/sessionModel";
import { conversationUsage, fmtDuration } from "../src/ui/chat/usageStats";

const env = (seq: number, ts: string, event: unknown, turn_id?: string) => ({ session_id: "s", seq, ts, ...(turn_id ? { turn_id } : {}), event }) as never;

/** One turn the way the engine sends it: only start/settle carry the turn id. */
function turn(m: ReturnType<typeof emptyModel>, n: number, startSec: number, secs: number, input: number, output: number) {
  const id = `trn_${n}`;
  const at = (s: number) => new Date(Date.UTC(2026, 0, 1, 10, 0, s)).toISOString();
  applyEvent(m, env(n * 10, at(startSec), { type: "turn.started", turn_id: id, message_id: `u${n}` }, id));
  applyEvent(m, env(n * 10 + 1, at(startSec + 1), { type: "part.started", kind: "text", message_id: `a${n}`, part_id: `p${n}` }));
  applyEvent(m, env(n * 10 + 2, at(startSec + secs), { type: "step.ended", finish: "stop", usage: { input_tokens: input, output_tokens: output }, cost: { usd: 0 } }));
  applyEvent(m, env(n * 10 + 3, at(startSec + secs), { type: "turn.settled", turn_id: id, status: "completed" }, id));
}

describe("conversation usage", () => {
  it("adds up every reply's tokens and time", () => {
    const m = emptyModel("s");
    turn(m, 1, 0, 3, 1000, 50);
    turn(m, 2, 10, 5, 2000, 150);
    const u = conversationUsage(m);
    expect(u).toMatchObject({ input: 3000, output: 200, replies: 2, ms: 8000, longestMs: 5000 });
  });

  it("formats long durations", () => {
    expect(fmtDuration(3400)).toBe("3.4s");
    expect(fmtDuration(72_000)).toBe("1m 12s");
    expect(fmtDuration(3_900_000)).toBe("1h 5m");
  });
});
