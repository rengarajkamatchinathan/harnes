import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionEventEnvelope } from "@genie/protocol";
import { applyEvent, drivenElsewhere, emptyModel, isBusy, loadSnapshot, siblingInfo } from "../src/state/sessionModel";

let seq = 0;
const env = (event: SessionEvent, durable = true): SessionEventEnvelope => ({ session_id: "s1", ts: "2026-01-01T00:00:00Z", turn_id: "t1", event, ...(durable ? { seq: ++seq } : { ref_seq: seq }) });

function snapshot() {
  const m = emptyModel("s1");
  loadSnapshot(m, {
    session: { session_id: "s1", kind: "chat", mode: "default", model: "m", head_seq: 0, status: "idle", created_at: "", updated_at: "" },
    path: [{ id: "u0", role: "user", created_at: "", parts: [{ type: "text", id: "p0", text: "hi" }] }],
    siblings: { u0: ["u0", "u0b"] },
  });
  return m;
}

describe("session view model", () => {
  it("streams a text part, then settles it", () => {
    const m = snapshot();
    applyEvent(m, env({ type: "message.admitted", message_id: "u1", delivery: "steer", parts: [{ type: "text", text: "question" }], role: "user", source: "user" }));
    applyEvent(m, env({ type: "turn.started", turn_id: "t1", message_id: "u1" }));
    applyEvent(m, env({ type: "part.started", kind: "text", message_id: "a1", part_id: "p1" }));
    const rev = m.messages[2].rev;
    applyEvent(m, env({ type: "part.delta", part_id: "p1", text: "Hel" }, false));
    applyEvent(m, env({ type: "part.delta", part_id: "p1", text: "lo" }, false));
    expect(m.messages[2].parts[0]).toMatchObject({ text: "Hello", streaming: true });
    expect(m.messages[2].rev).toBeGreaterThan(rev);
    applyEvent(m, env({ type: "part.ended", part_id: "p1", part: { type: "text", id: "p1", text: "Hello!", citations: [{ start: 0, end: 5, url: "https://a.example" }] } }));
    expect(m.messages[2].parts[0]).toMatchObject({ text: "Hello!", streaming: false });
    // A late delta after part.ended does nothing.
    expect(applyEvent(m, env({ type: "part.delta", part_id: "p1", text: "x" }, false))).toBe(false);
    expect(m.messages.map((x) => x.role)).toEqual(["user", "user", "assistant"]);
    expect(m.info?.head_seq).toBe(seq);
  });

  it("joins a part mid-stream under a live message", () => {
    const m = snapshot();
    applyEvent(m, env({ type: "part.delta", part_id: "px", text: "partial" }, false));
    expect(m.messages.at(-1)).toMatchObject({ id: "live:t1", role: "assistant" });
    expect(m.messages.at(-1)!.parts[0].text).toBe("partial");
  });

  it("tracks tools, asks, inbox, status and usage", () => {
    const m = snapshot();
    applyEvent(m, env({ type: "part.ended", part_id: "c1", part: { type: "tool_call", id: "c1", call_id: "call1", name: "shell", input: { command: "ls" } } }));
    applyEvent(m, env({ type: "tool.started", call_id: "call1", name: "shell", resource: "ls" }));
    const tools = m.tools;
    applyEvent(m, env({ type: "tool.progress", call_id: "call1", message: "1/2" }, false));
    expect(m.tools).not.toBe(tools); // new identity → rows re-render
    applyEvent(m, env({ type: "tool.ended", call_id: "call1", outcome: "ok", result_part_id: "r1" }));
    expect(m.tools.get("call1")).toMatchObject({ outcome: "ok", resultPartId: "r1", progress: null });

    applyEvent(m, env({ type: "permission.asked", ask: { id: "a1", kind: "permission", session_id: "s1", created_at: "" } }));
    expect(m.asks.has("a1")).toBe(true);
    applyEvent(m, env({ type: "permission.resolved", ask_id: "a1", by: "rule", decision: "once" }));
    expect(m.asks.size).toBe(0);

    applyEvent(m, env({ type: "inbox.changed", items: [{ id: "i1", delivery: "queue", preview: "next" }] }));
    expect(m.inbox).toHaveLength(1);
    applyEvent(m, env({ type: "status", state: "running", activity: "thinking" }, false));
    expect(isBusy(m)).toBe(true);
    applyEvent(m, env({ type: "usage.updated", context_tokens: 50, context_window: 100, cost: { usd: 0.1 }, session: {} }, false));
    expect(m.usage?.context_tokens).toBe(50);
    applyEvent(m, env({ type: "retry.scheduled", after_ms: 8000, attempt: 1, error_class: "rate_limit" }));
    expect(m.retry?.attempt).toBe(1);
    applyEvent(m, env({ type: "turn.settled", turn_id: "t1", status: "failed", error: { code: "E", message: "boom" } }));
    expect(m.retry).toBeNull();
    expect(m.error?.message).toBe("boom");
    applyEvent(m, env({ type: "turn.interrupted", turn_id: "t1", reason: "crash" }));
    expect(m.interrupted?.reason).toBe("crash");
  });

  it("knows the driver, branches and ignores unknown events", () => {
    const m = snapshot();
    applyEvent(m, env({ type: "lease.changed", driver: { client_instance_id: "cli_1", client_kind: "cli", label: "CLI" } }));
    expect(drivenElsewhere(m, "desktop_1")?.label).toBe("CLI");
    expect(drivenElsewhere(m, "cli_1")).toBeNull();
    expect(siblingInfo(m, "u0")).toEqual({ index: 0, ids: ["u0", "u0b"] });
    expect(applyEvent(m, env({ type: "Unknown" }))).toBe(false);
    expect(applyEvent({ ...m, sessionId: "other" }, env({ type: "status", state: "idle" }))).toBe(false);
    applyEvent(m, env({ type: "branch.switched", active_leaf_id: "x" }));
    expect(m.needsReload).toBe(true);
  });
});
