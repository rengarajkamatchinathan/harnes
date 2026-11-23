import { describe, expect, it } from "vitest";
import type { Part, UsageUpdated } from "@genie/protocol";
import type { MessageView, PartView } from "../src/state/sessionModel";
import { analyzeContext, contextLevel, estTokens, handoffPrompt, liveMessages, toolLabel } from "../src/ui/chat/contextTrim";
import { buildRows, rowIndexOf } from "../src/ui/chat/Transcript";
import { registerTranscriptScroll, scrollToMessage } from "../src/ui/chat/scrollBus";

let n = 0;
const part = (final: Part, text = ""): PartView => ({ id: "id" in final ? final.id : `p${++n}`, kind: final.type === "unknown" ? "unknown" : final.type, text, json: "", final, streaming: false });
const textPart = (text: string) => part({ type: "text", id: `p${++n}`, text }, text);
const msg = (id: string, role: "user" | "assistant", parts: PartView[], extra: Partial<MessageView> = {}): MessageView => ({ id, role, parentId: null, turnId: null, model: null, createdAt: "2026-01-01T00:00:00Z", parts, rev: 0, ...extra });
const usage = (context_tokens: number, context_window: number): UsageUpdated => ({ context_tokens, context_window, cost: { usd: 0 }, session: {} as UsageUpdated["session"] });

function conversation() {
  const call: Part = { type: "tool_call", id: "pc", call_id: "c1", name: "shell", input: { command: "npm test" } };
  const result: Part = { type: "tool_result", id: "pr", call_id: "c1", content: "x".repeat(40_000) };
  return [
    msg("u1", "user", [textPart("a".repeat(400))]),
    msg("a1", "assistant", [part(call), textPart("b".repeat(800))]),
    // The result lives in a later (tool) message: it is grouped with its call.
    msg("t1", "assistant", [part(result)]),
    msg("u2", "user", [textPart("p".repeat(8000))], { input: [{ type: "attachment", blob_id: "b1", mime: "image/png", name: "shot.png" }] }),
    msg("a2", "assistant", [part({ type: "reasoning", id: "pr2", text: "r".repeat(2000) }, "r".repeat(2000)), textPart("c".repeat(1200))]),
  ];
}

describe("context trim analysis", () => {
  it("estimates ≈ chars / 4", () => {
    expect(estTokens(0)).toBe(0);
    expect(estTokens(9)).toBe(3);
  });

  it("ranks the heaviest items with labels and categories", () => {
    const a = analyzeContext({ messages: conversation(), usage: null });
    expect(a.scaled).toBe(false);
    expect(a.ratio).toBeNull();
    const top = a.items[0];
    expect(top).toMatchObject({ messageId: "a1", category: "tool", label: "Tool output: shell `npm test`", huge: true });
    expect(top.tokens).toBeGreaterThan(10_000);
    expect(a.items.find((i) => i.label === "Pasted text")).toMatchObject({ messageId: "u2", category: "user", tokens: 2000 });
    expect(a.items.find((i) => i.category === "media")).toMatchObject({ label: "Image shot.png", tokens: 1600 });
    expect(a.items.find((i) => i.category === "reasoning")?.tokens).toBe(500);
    expect(a.items.find((i) => i.label === "Your message")?.tokens).toBe(100);
    expect(a.items.filter((i) => i.label === "Reply").map((i) => i.tokens)).toEqual([300, 200]);
    const sum = a.items.reduce((s, i) => s + i.tokens, 0);
    expect(a.total).toBe(sum);
    expect(a.items.reduce((s, i) => s + i.share, 0)).toBeCloseTo(1);
    expect(a.byCategory.system).toBe(0);
    for (let i = 1; i < a.items.length; i++) expect(a.items[i - 1].tokens).toBeGreaterThanOrEqual(a.items[i].tokens);
  });

  it("scales to the engine's count, keeping the overhead as instructions & tools", () => {
    const raw = analyzeContext({ messages: conversation(), usage: null }).total;
    const a = analyzeContext({ messages: conversation(), usage: usage(raw * 1.5, raw * 2) });
    expect(a.scaled).toBe(true);
    expect(a.ratio).toBeCloseTo(0.75);
    expect(Object.values(a.byCategory).reduce((s, v) => s + v, 0)).toBeCloseTo(raw * 1.5, -1);
    expect(a.total).toBe(raw * 1.5);
    // A tiny conversation with a big system prompt: estimates are scaled at most ×2, the rest is overhead.
    const small = analyzeContext({ messages: [msg("u", "user", [textPart("hello there")])], usage: usage(10_000, 100_000) });
    expect(small.items[0].tokens).toBe(6);
    expect(small.byCategory.system).toBe(10_000 - 6);
    expect(small.total).toBe(10_000);
  });

  it("only counts what follows the latest compaction", () => {
    const msgs = [...conversation(), msg("cm", "assistant", [part({ type: "compaction", id: "cp", summary: "s".repeat(400) })]), msg("u3", "user", [textPart("next")])];
    expect(liveMessages(msgs).map((m) => m.id)).toEqual(["cm", "u3"]);
    const a = analyzeContext({ messages: msgs, usage: null });
    expect(a.items.map((i) => i.label)).toEqual(["Summary of earlier messages", "Your message"]);
  });

  it("labels tools and grades the level", () => {
    expect(toolLabel("read", { path: "src/app.ts" })).toBe("Tool output: read src/app.ts");
    expect(toolLabel("bash", { command: "x".repeat(60) })).toMatch(/^Tool output: shell `x{39}…`$/);
    expect(contextLevel(null)).toBe("ok");
    expect(contextLevel(0.69)).toBe("ok");
    expect(contextLevel(0.74)).toBe("warn");
    expect(contextLevel(0.9)).toBe("critical");
  });

  it("builds a hand-off prompt from the latest messages within a budget", () => {
    const p = handoffPrompt(conversation(), 2000, "Tests");
    expect(p).toContain('("Tests")');
    expect(p).toContain("Genie: ccc");
    expect(p).not.toContain("xxxx"); // tool output left out
    expect(p).not.toContain("rrrr"); // reasoning left out
    expect(p.length).toBeLessThan(2600);
    expect(p.indexOf("User: ppp")).toBeLessThan(p.indexOf("Genie: ccc"));
  });
});

describe("scroll to a message", () => {
  it("finds the row of a message, its tool group, or the closest earlier row", () => {
    const msgs = conversation();
    const rows = buildRows(msgs);
    expect(rowIndexOf(msgs, rows, "u1")).toBe(0);
    expect(rowIndexOf(msgs, rows, "a1")).toBe(1);
    expect(rows[rowIndexOf(msgs, rows, "u2")].key).toBe("u2");
    expect(rowIndexOf(msgs, rows, "missing")).toBe(-1);
  });

  it("routes requests to the mounted transcript of that session", () => {
    const seen: string[] = [];
    const off = registerTranscriptScroll("s1", (id) => seen.push(id));
    expect(scrollToMessage("s1", "m1")).toBe(true);
    expect(scrollToMessage("s2", "m1")).toBe(false);
    off();
    expect(scrollToMessage("s1", "m2")).toBe(false);
    expect(seen).toEqual(["m1"]);
  });
});

describe("context meter over the mock engine", () => {
  const until = async (f: () => boolean, ms = 4000) => {
    const end = Date.now() + ms;
    while (!f()) {
      if (Date.now() > end) throw new Error("timed out");
      await new Promise((r) => setTimeout(r, 5));
    }
  };

  it("fills Genie Test's small window with logs, then compacts it", async () => {
    const { createMockPlatform } = await import("../src/platform/mock");
    const { App } = await import("../src/state/app");
    const app = new App(createMockPlatform({ tickMs: 0 }));
    await app.start();
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await app.client.session.update({ session_id: id, model: "mock/genie-test" });
    for (let i = 0; i < 2; i++) {
      const replies = s.model.messages.filter((m) => m.role === "assistant").length;
      await s.send([{ type: "text", text: "show me the logs" }]);
      await until(() => s.model.status === "idle" && s.model.messages.filter((m) => m.role === "assistant").length > replies && !!s.model.usage);
    }
    const a = analyzeContext(s.model);
    expect(a.window).toBe(32000);
    expect(contextLevel(a.ratio)).not.toBe("ok");
    expect(a.items[0]).toMatchObject({ category: "tool", label: "Tool output: shell `npm test -- --reporter=verbose`", huge: true });
    const before = s.model.usage!.context_tokens;
    await app.client.session.compact({ session_id: id, focus: "the failing tests" });
    await until(() => s.model.messages.some((m) => m.parts.some((p) => p.kind === "compaction" && !!p.final)) && s.model.usage!.context_tokens < before);
    expect(contextLevel(analyzeContext(s.model).ratio)).toBe("ok");
    expect(analyzeContext(s.model).items[0].label).toBe("Summary of earlier messages");
  });
});
