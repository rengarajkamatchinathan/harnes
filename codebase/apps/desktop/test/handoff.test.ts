import { describe, expect, it } from "vitest";
import type { Part } from "@genie/protocol";
import type { MessageView, PartView } from "../src/state/sessionModel";
import {
  buildHandoff,
  DEFAULT_HANDOFF_OPTIONS,
  DIFF_MAX,
  handoffContinuePrompt,
  handoffFileName,
  parseHandoff,
  redact,
  TOOL_RESULT_MAX,
  TRUNCATED,
  type HandoffOptions,
} from "../src/state/handoff";

let n = 0;
const part = (final: Part, text = ""): PartView => ({ id: "id" in final ? final.id : `p${++n}`, kind: final.type === "unknown" ? "unknown" : final.type, text, json: "", final, streaming: false });
const textPart = (text: string) => part({ type: "text", id: `p${++n}`, text }, text);
const msg = (id: string, role: "user" | "assistant", parts: PartView[], extra: Partial<MessageView> = {}): MessageView => ({ id, role, parentId: null, turnId: null, model: null, createdAt: "2026-01-01T00:00:00Z", parts, rev: 0, ...extra });

const DIFF = ["diff --git a/src/a.ts b/src/a.ts", "--- a/src/a.ts", "+++ b/src/a.ts", "@@ -1,2 +1,2 @@", "-old", "+new", "+more", ""].join("\n");

function input(result = "PASS 3 tests") {
  return {
    title: "Fix tests",
    kind: "code",
    model: "gpt-x",
    place: { name: "harnes", path: "C:\\work\\harnes" },
    messages: [
      msg("u1", "user", [textPart("Please fix the tests")]),
      msg("a1", "assistant", [
        part({ type: "reasoning", id: "r1", text: "thinking" }, "thinking"),
        part({ type: "tool_call", id: "c", call_id: "c1", name: "shell", input: { command: "npm test" } }),
        textPart("Done."),
      ], { usage: { input: 10, output: 5, reasoning: 0, cacheRead: 0, usd: 0.01, steps: 1 } }),
      msg("t1", "assistant", [part({ type: "tool_result", id: "tr", call_id: "c1", content: result })]),
      msg("s1", "user", [textPart("background job finished")], { synthetic: true }),
    ],
    tools: new Map([["c1", { outcome: "ok" as const }]]),
    diff: DIFF,
    now: new Date("2026-02-03T04:05:06Z"),
  };
}

const all: HandoffOptions = { includeTools: true, includeToolOutput: true, includeReasoning: true, includeDiff: true, fullPath: true };
const none: HandoffOptions = { includeTools: false, includeToolOutput: false, includeReasoning: false, includeDiff: false, fullPath: false };

describe("buildHandoff", () => {
  it("includes everything when asked", () => {
    const h = buildHandoff(input(), all);
    expect(h).toMatchObject({ format: "genie-handoff", version: 1, exported_at: "2026-02-03T04:05:06.000Z", title: "Fix tests", kind: "code", model: "gpt-x" });
    expect(h.place).toEqual({ name: "harnes", path: "C:\\work\\harnes" });
    expect(h.messages).toHaveLength(2);
    expect(h.messages[0]).toMatchObject({ role: "user", text: "Please fix the tests", tools: [] });
    expect(h.messages[1]).toMatchObject({ role: "assistant", text: "Done.", reasoning: "thinking", usage: { input: 10, output: 5, usd: 0.01 } });
    expect(h.messages[1].tools).toEqual([{ name: "shell", summary: expect.stringContaining("npm test"), outcome: "ok", result: "PASS 3 tests" }]);
    expect(h.diff).toBe(DIFF);
    expect(h.stats).toEqual({ messages: 2, tools: 1, diffFiles: 1, added: 2, removed: 1 });
    expect(h.redactions).toBe(0);
  });

  it("leaves out optional content by default / when off", () => {
    const d = buildHandoff(input(), DEFAULT_HANDOFF_OPTIONS);
    expect(d.place).toEqual({ name: "harnes" });
    expect(d.messages[1].reasoning).toBeUndefined();
    expect(d.messages[1].tools[0].result).toBeUndefined();
    const h = buildHandoff(input(), none);
    expect(h.messages[1].tools).toEqual([]);
    expect(h.diff).toBeUndefined();
    expect(h.stats).toEqual({ messages: 2, tools: 1 });
  });

  it("truncates tool output and diffs", () => {
    const h = buildHandoff({ ...input("x".repeat(TOOL_RESULT_MAX + 50)), diff: DIFF + "+".repeat(DIFF_MAX) }, all);
    const r = h.messages[1].tools[0].result!;
    expect(r.endsWith(TRUNCATED)).toBe(true);
    expect(r.length).toBeLessThan(TOOL_RESULT_MAX + 20);
    expect(h.diff!.endsWith(TRUNCATED)).toBe(true);
    expect(h.diff!.length).toBeLessThan(DIFF_MAX + 20);
  });

  it("redacts secrets in every text field and counts them", () => {
    const i = input("token=abcdefgh12345678");
    i.title = "key sk-abcdefghijklmnop1234";
    i.messages[0] = msg("u1", "user", [textPart("use AKIAABCDEFGHIJKLMNOP")]);
    const h = buildHandoff({ ...i, diff: `${DIFF}+password: hunter2hunter2\n` }, all);
    expect(h.title).toBe("key [redacted]");
    expect(h.messages[0].text).toBe("use [redacted]");
    expect(h.messages[1].tools[0].result).toBe("token=[redacted]");
    expect(h.diff).toContain("+password: [redacted]");
    expect(h.redactions).toBe(4);
  });
});

describe("redact", () => {
  const cases: [string, string][] = [
    ["sk-proj_ABCDEFGHIJKLmnop", "[redacted]"],
    ["AKIAIOSFODNN7EXAMPLE", "[redacted]"],
    ["ghp_abcdefghijklmnopqrstuvwxyz0123", "[redacted]"],
    ["xoxb-1234567890-abcdef", "[redacted]"],
    ["Authorization: Bearer abc.def-ghi_jkl~mno", "Authorization: Bearer [redacted]"],
    ["-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----", "[redacted]"],
    ['api_key = "abcd1234efgh"', 'api_key = "[redacted]"'],
    ["API-KEY: zzzzzzzz", "API-KEY: [redacted]"],
    ["secret=s3cr3tvalue", "secret=[redacted]"],
    ["passwd: correcthorse", "passwd: [redacted]"],
  ];
  it.each(cases)("redacts %s", (src, out) => {
    expect(redact(src)).toEqual({ text: out, count: 1 });
  });

  it("does not count a key/value whose value is already redacted", () => {
    expect(redact("token = sk-abcdefghijklmnopqrst")).toEqual({ text: "token = [redacted]", count: 1 });
  });

  it("leaves ordinary text alone", () => {
    const plain = "Set max_tokens: 4096, password: short, the task-runner skeleton and AKIA lowercase akiaabcdefghijklmnop.";
    expect(redact(plain)).toEqual({ text: plain, count: 0 });
  });
});

describe("parseHandoff", () => {
  it("round-trips a built handoff", () => {
    const h = buildHandoff(input(), all);
    const r = parseHandoff(JSON.stringify(h));
    expect(r).toEqual({ ok: true, handoff: h });
  });

  it("rejects bad input", () => {
    expect(parseHandoff("not json")).toMatchObject({ ok: false });
    expect(parseHandoff("[]")).toMatchObject({ ok: false });
    expect(parseHandoff(JSON.stringify({ format: "other", version: 1 }))).toMatchObject({ ok: false });
    expect(parseHandoff(JSON.stringify({ format: "genie-handoff", version: 2 }))).toMatchObject({ ok: false, error: expect.stringContaining("newer") });
  });

  it("fills defaults for an older file with missing fields", () => {
    const r = parseHandoff(JSON.stringify({ format: "genie-handoff", messages: [{ role: "user", text: "hi" }, { text: "hello", tools: [{ name: "shell" }] }], diff: DIFF }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.handoff).toMatchObject({ version: 1, title: "Untitled", kind: "chat", model: "", place: { name: null }, redactions: 0 });
    expect(r.handoff.messages).toEqual([
      { role: "user", created_at: "", text: "hi", tools: [] },
      { role: "assistant", created_at: "", text: "hello", tools: [{ name: "shell", summary: "", outcome: null }] },
    ]);
    expect(r.handoff.stats).toEqual({ messages: 2, tools: 1, diffFiles: 1, added: 2, removed: 1 });
  });
});

describe("continue prompt and file name", () => {
  it("includes the turns and the diff stats", () => {
    const p = handoffContinuePrompt(buildHandoff(input(), all));
    expect(p).toContain('Genie handoff ("Fix tests")');
    expect(p).toContain("User: Please fix the tests");
    expect(p).toContain("Genie: Done.");
    expect(p).toContain("changed 1 file (+2 −1)");
    expect(p).toContain("- src/a.ts (+2 −1)");
  });

  it("makes a safe file name", () => {
    expect(handoffFileName('a/b: "c"?')).toBe("a b c.genie.json");
    expect(handoffFileName("  ")).toBe("handoff.genie.json");
  });
});
