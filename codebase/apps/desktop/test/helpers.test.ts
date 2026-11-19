import { describe, expect, it } from "vitest";
import type { SessionSummary } from "@genie/protocol";
import { BlockSplitter, renderSettled, renderTail, splitAll, withCitations } from "../src/markdown/markdown";
import { coerce, controlFor, keySchema, sectionOf, uiKeys, matches } from "../src/ui/settings/schema";
import { groupReads, headTail, summarizeTool } from "../src/ui/chat/toolInfo";
import { describeRules, permissionHeading } from "../src/ui/chat/AskCard";
import { keyAction } from "../src/ui/chat/Composer";
import { groupByDate } from "../src/ui/Sidebar";
import { formFields } from "../src/ui/chat/AskCard";
import { parseArtifactMessage } from "../src/ui/panel/ArtifactPanel";
import { bannerState, copyChord, isPasteChord, keyText, mapPoint, modifiers } from "../src/ui/panel/BrowserPanel";
import { sideBySide, splitDiff } from "../src/ui/panel/CodePanels";
import { panelTabs } from "../src/ui/panel/RightPanel";
import { nextStep } from "../src/ui/FirstRun";
import { plural, t } from "../src/i18n";
import { rejectReason, toBase64, fromBase64, MAX_FILE_BYTES } from "../src/state/attachments";

describe("markdown renderer (05 §7)", () => {
  it("splits blocks incrementally, keeping fences and math whole", () => {
    const s = new BlockSplitter();
    s.push("# Title\n\npara one\n\n```ts\nconst a = 1;\n\nconst b = 2;\n");
    expect(s.settled).toEqual(["# Title", "para one"]);
    expect(s.tail()).toContain("const b");
    s.push("```\n\n$$\nx\n\ny\n$$\n\nend");
    expect(s.settled).toHaveLength(4);
    s.finish();
    expect(s.settled.at(-1)).toBe("end");
    expect(splitAll("a\n\nb")).toEqual(["a", "b"]);
  });

  it("escapes raw HTML and neutralises links and images", () => {
    expect(renderSettled("<script>alert(1)</script>")).not.toContain("<script>");
    const link = renderSettled("[x](https://example.com)");
    expect(link).toContain('data-href="https://example.com"');
    expect(link).not.toMatch(/\shref="https:/);
    expect(renderSettled("[x](javascript:alert(1))")).not.toContain("javascript:");
    expect(renderSettled("![pic](https://evil.example/p.png)")).not.toContain("<img");
    expect(renderTail("```rust\nfn main() {}\n```")).toContain("hljs");
  });

  it("inserts citation markers", () => {
    const md = withCitations("Alpha beta.", [{ end: 5 }, { end: 11 }]);
    expect(md).toBe("Alpha⟦1⟧ beta.⟦2⟧");
    expect(renderSettled(md)).toContain('data-cite="1"');
  });
});

describe("settings schema (D23)", () => {
  const schema = {
    type: "object",
    properties: { memory: { anyOf: [{ $ref: "#/$defs/Toggle" }, { type: "null" }] }, theme: { $ref: "#/$defs/Theme" }, subagents: { type: "object", properties: { maxDepth: { type: ["integer", "null"], minimum: 0, maximum: 5 } } }, permissions: { type: "object", properties: { allow: { type: "array", items: { type: "string" } } } } },
    $defs: { Toggle: { type: "object", properties: { enabled: { type: ["boolean", "null"], description: "On/off" } } }, Theme: { oneOf: [{ const: "light" }, { const: "dark" }] } },
  };
  it("resolves refs and Option<T> for dotted keys", () => {
    expect(controlFor(keySchema(schema, "memory.enabled"))).toEqual({ kind: "boolean" });
    expect(controlFor(keySchema(schema, "theme"))).toEqual({ kind: "enum", options: ["light", "dark"] });
    expect(controlFor(keySchema(schema, "subagents.maxDepth"))).toMatchObject({ kind: "number", integer: true, max: 5 });
    expect(controlFor(keySchema(schema, "permissions.allow"))).toEqual({ kind: "list" });
    expect(keySchema(schema, "nope.x")).toBeUndefined();
  });
  it("coerces and validates", () => {
    expect(coerce({ kind: "number", integer: true, max: 5 }, "3")).toBe(3);
    expect(() => coerce({ kind: "number", integer: true, max: 5 }, "9")).toThrow();
    expect(coerce({ kind: "list" }, "a\n\nb ")).toEqual(["a", "b"]);
    expect(() => coerce({ kind: "json" }, "{")).toThrow();
  });
  it("maps keys to sections and searches", () => {
    expect(sectionOf("memory.enabled")).toBe("genie");
    expect(sectionOf("telemetry.usage")).toBe("privacy");
    expect(sectionOf("permissions.allow")).toBe("permissions");
    const metas = uiKeys({ keys: { theme: { title: "Theme", scopes_allowed: ["user"], restart_required: true } } });
    expect(metas[0]).toMatchObject({ key: "theme", restart: true, scopes: ["user"] });
    expect(matches("them", metas[0], undefined)).toBe(true);
    expect(matches("zzz", metas[0], undefined)).toBe(false);
  });
});

describe("chat helpers", () => {
  it("summarises tool rows (05 §2.2, D54)", () => {
    expect(summarizeTool("mcp__github__create_issue", {}, null).server).toBe("github");
    expect(summarizeTool("web_fetch", { url: "https://a.example" }, "ok").title).toBe("Fetched https://a.example");
    expect(summarizeTool("shell", { command: "cargo test" }, "error", "test result: 2 failed").title).toBe("Ran `cargo test` · 2 failed");
    expect(summarizeTool("edit", { path: "src/a.rs", diff: "+a\n+b\n-c\n" }, "ok").title).toBe("Edited src/a.rs +2 −1");
    expect(summarizeTool("memory_add", {}, "ok").memory).toBe(true);
    expect(groupReads(["Read a", "Read b", "Ran x", "Read c"])).toEqual(["Read 2 files", "Ran x", "Read 1 file"]);
    expect(headTail(Array.from({ length: 100 }, (_, i) => String(i)).join("\n")).hidden).toBe(60);
  });

  it("maps keys: Enter sends/steers, Alt+Enter queues while busy, Shift+Enter newline (SA-06)", () => {
    const k = (o: Partial<{ shiftKey: boolean; altKey: boolean }>, busy: boolean) => keyAction({ key: "Enter", shiftKey: false, altKey: false, ...o }, busy);
    expect(k({}, false)).toBe("send");
    expect(k({}, true)).toBe("send");
    expect(k({ altKey: true }, true)).toBe("queue");
    expect(k({ altKey: true }, false)).toBe("newline");
    expect(k({ shiftKey: true }, true)).toBe("newline");
    expect(keyAction({ key: "Enter", shiftKey: false, altKey: false, isComposing: true }, false)).toBeNull();
  });

  it("groups recents by date with pinned first", () => {
    const now = new Date("2026-11-17T12:00:00");
    const s = (id: string, updated_at: string, pinned = false): SessionSummary => ({ session_id: id, kind: "chat", status: "idle", updated_at, pinned });
    const g = groupByDate([s("a", "2026-11-17T09:00:00"), s("b", "2026-11-16T09:00:00"), s("c", "2026-11-12T09:00:00"), s("d", "2026-01-01T00:00:00"), s("e", "2026-01-02T00:00:00", true)], now);
    expect(g.map(([k, v]) => [k, v.map((x) => x.session_id)])).toEqual([
      ["pinned", ["e"]],
      ["today", ["a"]],
      ["yesterday", ["b"]],
      ["week", ["c"]],
      ["older", ["d"]],
    ]);
  });

  it("builds elicitation forms with masked sensitive fields", () => {
    const f = formFields({ properties: { name: { type: "string" }, token: { type: "string" }, n: { type: "integer" }, color: { enum: ["r", "g"] } }, required: ["name"] }, ["token"]);
    expect(f.map((x) => [x.name, x.type, x.required, x.sensitive])).toEqual([
      ["name", "string", true, false],
      ["token", "string", false, true],
      ["n", "integer", false, false],
      ["color", "enum", false, false],
    ]);
  });

  it("checks attachment limits (SA-27) and base64", () => {
    expect(rejectReason({ size: MAX_FILE_BYTES + 1 }, 0)).toMatch(/32 MB/);
    expect(rejectReason({ size: 1 }, 20)).toMatch(/20 files/);
    expect(rejectReason({ size: 1 }, 0)).toBeNull();
    const bytes = new Uint8Array([0, 1, 254, 255, 65]);
    expect(fromBase64(toBase64(bytes))).toEqual(bytes);
  });
});

describe("panels", () => {
  it("accepts only the fixed artifact postMessage schema (05 §8)", () => {
    expect(parseArtifactMessage({ type: "ready" })).toEqual({ type: "ready" });
    expect(parseArtifactMessage({ type: "resize", height: 300 })).toEqual({ type: "resize", height: 300 });
    expect(parseArtifactMessage({ type: "resize", height: "x" })).toBeNull();
    expect(parseArtifactMessage({ type: "runtime_error", message: "e", stack: 1 })).toEqual({ type: "runtime_error", message: "e", stack: "" });
    expect(parseArtifactMessage({ type: "invoke", cmd: "host_request" })).toBeNull();
    expect(parseArtifactMessage("ready")).toBeNull();
  });

  it("maps canvas points to CSS pixels of the viewport (SA-24)", () => {
    // A 1280×800 CSS viewport drawn 640 px wide: 2 CSS px per client px, whatever the JPEG scale.
    const p = mapPoint(150, 100, { left: 50, top: 0, width: 640, height: 400 }, { w: 1280, h: 800 });
    expect(p).toEqual({ x: 200, y: 200 });
    // Clamped to the viewport.
    expect(mapPoint(0, 900, { left: 50, top: 0, width: 640, height: 400 }, { w: 1280, h: 800 })).toEqual({ x: 0, y: 800 });
    expect(modifiers({ altKey: true, ctrlKey: false, metaKey: false, shiftKey: true })).toBe(9);
  });

  it("derives the browser banner and key text", () => {
    expect(bannerState(null, true)).toBe("idle");
    expect(bannerState({ control: "agent" }, true)).toBe("agent");
    expect(bannerState({ control: "agent" }, false)).toBe("idle");
    expect(bannerState({ control: "user" }, false)).toBe("user");
    expect(bannerState({ control: "handoff", reason: "log in" }, true)).toBe("handoff");
    expect(bannerState({ control: "waiting_for_approval" }, true)).toBe("waiting");
    expect(keyText({ key: "a", ctrlKey: false, metaKey: false })).toBe("a");
    expect(keyText({ key: " ", ctrlKey: false, metaKey: false })).toBe(" ");
    expect(keyText({ key: "a", ctrlKey: false, metaKey: true })).toBeNull();
    expect(keyText({ key: "Enter", ctrlKey: false, metaKey: false })).toBeNull();
    expect(isPasteChord({ key: "v", ctrlKey: false, metaKey: true })).toBe(true);
    expect(isPasteChord({ key: "V", ctrlKey: true, metaKey: false })).toBe(true);
    expect(isPasteChord({ key: "v", ctrlKey: false, metaKey: false })).toBe(false);
    const k = (key: string, mods: Partial<Record<"ctrlKey" | "metaKey" | "altKey" | "shiftKey", boolean>> = {}) => ({ key, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...mods });
    expect(copyChord(k("c", { metaKey: true }))).toBe("copy");
    expect(copyChord(k("X", { ctrlKey: true }))).toBe("cut");
    expect(copyChord(k("c", { ctrlKey: true, shiftKey: true }))).toBeNull();
    expect(copyChord(k("c"))).toBeNull();
  });

  it("splits diffs per file and side by side", () => {
    const d = "diff --git a/x b/x\n--- a/x\n+++ b/x\n a\n-b\n+c\ndiff --git a/y b/y\n+z\n";
    expect(splitDiff(d).map((f) => f.path)).toEqual(["x", "y"]);
    expect(sideBySide(splitDiff(d)[0].text).slice(0, 2)).toEqual([
      ["a", "a"],
      ["b", "c"],
    ]);
    expect(panelTabs("chat")).toEqual(["artifact", "browser"]);
    expect(panelTabs("code")).toEqual(["changes", "files", "terminal", "browser"]);
  });

  it("walks first run, skipping import when nothing is found (05 §5)", () => {
    expect(nextStep("welcome", false)).toBe("privacy");
    expect(nextStep("provider", false)).toBe("memory");
    expect(nextStep("provider", true)).toBe("import");
    expect(nextStep("memory", true)).toBeNull();
  });

  it("formats strings through keys", () => {
    expect(t("conn.busy", { n: 3 })).toBe("Genie is finishing 3 tasks before updating.");
    expect(plural(1, { one: "# file", other: "# files" })).toBe("1 file");
    expect(plural(2, { one: "# file", other: "# files" })).toBe("2 files");
  });
});

describe("turn indicator", () => {
  const base = { status: "running", tools: new Map(), messages: [] } as unknown as import("../src/state/sessionModel").SessionModel;
  it("labels tool activity in the present tense", async () => {
    const { toolActivity } = await import("../src/ui/chat/toolInfo");
    expect(toolActivity("bash")).toBe("Running a command");
    expect(toolActivity("mcp__github__list")).toBe("Using github");
    expect(toolActivity("load_skill")).toBe("Using a skill");
  });
  it("shows a running tool, thinking, mark-only while writing, else working", async () => {
    const { turnLabel } = await import("../src/ui/chat/Transcript");
    expect(turnLabel(base)).toBe("Working on it");
    expect(turnLabel({ ...base, tools: new Map([["c", { name: "web_search", outcome: null }]]) } as never)).toBe("Searching the web");
    const msg = (kind: string) => ({ role: "assistant", parts: [{ kind, streaming: true }] });
    expect(turnLabel({ ...base, messages: [msg("reasoning")] } as never)).toBe("Thinking");
    expect(turnLabel({ ...base, messages: [msg("text")] } as never)).toBe("");
    expect(turnLabel({ ...base, status: "compacting" } as never)).toContain("Summarising");
  });
});

describe("transcript rows", () => {
  const part = (id: string, kind: string, extra: Record<string, unknown> = {}) => ({ id, kind, text: "", json: "", final: null, streaming: false, ...extra });
  const call = (id: string, name: string) => part(id, "tool_call", { final: { type: "tool_call", id, call_id: id, name, input: {} } });
  const msg = (id: string, role: string, parts: unknown[]) => ({ id, role, parentId: null, turnId: "t", model: null, createdAt: "", parts, rev: 0 });
  it("drops result-only steps and folds consecutive tool-only steps", async () => {
    const { buildRows } = await import("../src/ui/chat/Transcript");
    const rows = buildRows([
      msg("u", "user", [part("u1", "text", { text: "desc this repo" })]),
      msg("a1", "assistant", [call("c1", "read")]),
      msg("r1", "tool", [part("r1p", "tool_result")]),
      msg("a2", "assistant", [call("c2", "read"), call("c3", "list")]),
      msg("r2", "tool", [part("r2p", "tool_result")]),
      msg("a3", "assistant", [part("x", "text", { text: "It is a Tauri app." })]),
    ] as never);
    expect(rows.map((r) => r.kind)).toEqual(["msg", "tools", "msg"]);
    expect(rows[1].kind === "tools" && rows[1].parts.length).toBe(3);
  });
  it("summarises a folded group", async () => {
    const { toolGroupSummary } = await import("../src/ui/chat/toolInfo");
    expect(toolGroupSummary(["read", "read", "list", "bash"])).toBe("Read 2 files, searched the code, ran a command");
    expect(toolGroupSummary(["mcp__x__y"])).toBe("Used a tool");
  });
});

describe("streaming table head", () => {
  it("adds a provisional separator until the real one arrives", async () => {
    const { completeTableHead, renderTail } = await import("../src/markdown/markdown");
    expect(completeTableHead("| Step | Cost |")).toBe("| Step | Cost |\n| --- | --- |");
    expect(completeTableHead("| Step | Cost |\n|--")).toBe("| Step | Cost |\n| --- | --- |");
    expect(completeTableHead("| a | b |\n|---|---|\n| 1 | 2 |")).toBe("| a | b |\n|---|---|\n| 1 | 2 |");
    expect(completeTableHead("plain text")).toBe("plain text");
    expect(renderTail("| Step | Cost |")).toContain("<table");
  });
});

describe("permission card text", () => {
  it("describes what an always saves in plain words", () => {
    expect(describeRules(["Bash(ps:*)", "Bash(egrep:*)"])).toBe("don’t ask again for “ps” and “egrep” commands");
    expect(describeRules(["Bash(npm run build)"])).toBe("don’t ask again for “npm run build”");
    expect(describeRules(["Read(//etc/**)"])).toBe("allow reading “/etc/”");
    expect(describeRules(["Edit(/src/**)"])).toBe("allow edits in “src/”");
    expect(describeRules(["Edit(/**)"])).toBe("allow all edits");
    expect(describeRules(["WebFetch(domain:docs.rs)"])).toBe("allow fetching from “docs.rs”");
    expect(describeRules(["mcp__github__create_issue"])).toBe("always allow “create_issue”");
    expect(describeRules(["Bash(ls:*)", "Read(//etc/**)"])).toBeNull();
  });
  it("titles asks by tool", () => {
    const base = { id: "a", session_id: "s", kind: "permission" as const, created_at: "" };
    expect(permissionHeading({ ...base, tool: "shell", preview: { type: "command", command: "ls", cwd: "/a/AIO" } })).toMatchObject({ title: "Run a command", sub: "in AIO" });
    expect(permissionHeading({ ...base, tool: "edit", preview: { type: "diff", path: "/a/src/x.ts", unified: "" } })).toMatchObject({ title: "Edit x.ts", sub: "/a/src" });
    expect(permissionHeading({ ...base, tool: "mcp__github__create_issue" })).toMatchObject({ title: "Use create_issue", sub: "from github" });
  });
});
