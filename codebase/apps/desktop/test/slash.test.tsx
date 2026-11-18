// The `/` menu (05 §2.3, 06 §9.1, D58): ranking, parsing, the engine's command list over the
// mock platform, client built-ins, and the menu markup.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { CommandInfo } from "@genie/protocol";
import { createMockPlatform } from "../src/platform/mock";
import { errorKind } from "../src/platform";
import { App } from "../src/state/app";
import { SlashMenu, runClientCommand } from "../src/ui/chat/SlashMenu";
import { findCommand, hintAfter, needsArgs, parseSlash, pushRecent, rankCommands, slashQuery } from "../src/ui/chat/slash";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

const cmd = (name: string, source: CommandInfo["source"], extra: Partial<CommandInfo> = {}): CommandInfo => ({ name, description: `${name} desc`, source, route: source === "builtin" ? "client" : "prompt", ...extra });

const LIST: CommandInfo[] = [
  cmd("help", "builtin"),
  cmd("model", "builtin", { args_hint: "[model]" }),
  cmd("mcp", "builtin", { aliases: ["connectors"] }),
  cmd("plan", "builtin", { args_hint: "[<description>]" }),
  cmd("review", "user"),
  cmd("pdf", "skill"),
  cmd("files:summarize", "mcp_prompt", { args_hint: "<path> [style]" }),
];

describe("slash menu logic", () => {
  it("opens only for a name typed at the very start", () => {
    expect(slashQuery("/")).toBe("");
    expect(slashQuery("/mo")).toBe("mo");
    expect(slashQuery("/model ")).toBeNull();
    expect(slashQuery("hi /model")).toBeNull();
    expect(slashQuery(" /model")).toBeNull();
  });

  it("parses a command and its arguments", () => {
    expect(parseSlash("/model opus")).toEqual({ name: "model", args: "opus" });
    expect(parseSlash("/plan  add a cache\nwith tests ")).toEqual({ name: "plan", args: "add a cache\nwith tests" });
    expect(parseSlash("/usr/bin/env is here")).toBeNull();
    expect(parseSlash("hello")).toBeNull();
  });

  it("orders recently used first, then built-ins, commands and skills, connector prompts", () => {
    expect(rankCommands(LIST, "", []).map((c) => c.name)).toEqual(["help", "model", "mcp", "plan", "review", "pdf", "files:summarize"]);
    expect(rankCommands(LIST, "", ["pdf", "plan"]).map((c) => c.name).slice(0, 3)).toEqual(["pdf", "plan", "help"]);
  });

  it("filters by name and alias, prefix matches first", () => {
    expect(rankCommands(LIST, "con", []).map((c) => c.name)).toEqual(["mcp"]);
    // Prefix matches (plan, pdf) before substring matches (help, mcp).
    expect(rankCommands(LIST, "p", []).map((c) => c.name)).toEqual(["plan", "pdf", "help", "mcp"]);
    expect(rankCommands(LIST, "sum", []).map((c) => c.name)).toEqual(["files:summarize"]);
    expect(rankCommands(LIST, "zzz", [])).toEqual([]);
  });

  it("keeps a short recent list and resolves aliases", () => {
    let r: string[] = [];
    for (const n of ["a", "b", "c", "a"]) r = pushRecent(r, n);
    expect(r).toEqual(["a", "c", "b"]);
    expect(pushRecent(Array.from({ length: 20 }, (_, i) => `x${i}`), "y")).toHaveLength(8);
    expect(findCommand(LIST, "connectors")?.name).toBe("mcp");
  });

  it("waits for a required argument and shows the hint until arguments are typed", () => {
    const fs = findCommand(LIST, "files:summarize")!;
    const plan = findCommand(LIST, "plan")!;
    expect(needsArgs(fs, "")).toBe(true);
    expect(needsArgs(fs, "README.md")).toBe(false);
    expect(needsArgs(plan, "")).toBe(false);
    expect(hintAfter(plan, "")).toBe("[<description>]");
    expect(hintAfter(plan, "x")).toBeNull();
  });

  it("renders names only, the selected row's description as a tooltip, and an empty state", () => {
    const html = renderToStaticMarkup(<SlashMenu rows={LIST.slice(0, 3)} loading={false} selected={1} onHover={() => {}} onPick={() => {}} />);
    expect(html).toContain('role="listbox"');
    expect(html).toMatch(/aria-selected="true"[^>]*>model</);
    expect(html).not.toContain("BUILT-IN");
    const empty = renderToStaticMarkup(<SlashMenu rows={[]} loading={false} selected={0} onHover={() => {}} onPick={() => {}} />);
    expect(empty).toContain("No command matches. Enter sends it as a message.");
  });
});

describe("slash commands over the mock platform", () => {
  const start = async () => {
    const app = new App(createMockPlatform({ tickMs: 0, reply: () => "ok" }));
    await app.start();
    await until(() => app.models.length > 0);
    return app;
  };

  it("lists the engine's commands for the session kind", async () => {
    const app = await start();
    const chat = await app.client.command.list({ kind: "chat" });
    const names = chat.commands.map((c) => c.name);
    expect(names).toContain("model");
    expect(names).toContain("review");
    expect(names).toContain("files:summarize");
    expect(names).not.toContain("compact");
    const code = await app.client.command.list({ kind: "code" });
    expect(code.commands.find((c) => c.name === "init")?.route).toBe("prompt");
    expect(code.commands.find((c) => c.name === "compact")?.route).toBe("client");
  });

  it("rejects a client built-in sent as a prompt and sends prompt commands as command parts", async () => {
    const app = await start();
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await expect(s.send([{ type: "text", text: "/mcp" }])).rejects.toSatisfy((e: unknown) => errorKind(e) === "command_not_prompt");
    await s.send([{ type: "command", name: "review", args: "src/" }]);
    await until(() => s.model.messages.some((m) => m.input?.some((p) => p.type === "command" && p.name === "review")));
  });

  it("runs client built-ins without a model turn", async () => {
    const app = await start();
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    const before = s.model.messages.length;

    const other = app.usableModels.find((m) => m.id !== (s.model.info?.model ?? ""));
    if (other) {
      expect(await runClientCommand(app, s, "chat", "model", other.id)).toBeNull();
      expect(app.ui.model).toBe(other.id);
    }
    const choice = await runClientCommand(app, s, "chat", "model", "");
    expect(choice?.kind).toBe("choice");

    const n = app.searchRequest;
    await runClientCommand(app, s, "chat", "resume", "");
    expect(app.searchRequest).toBe(n + 1);

    await runClientCommand(app, s, "chat", "memory", "");
    expect(app.ui.view).toBe("settings");
    expect(app.ui.settingsSection).toBe("genie");

    expect((await runClientCommand(app, s, "chat", "help", ""))?.kind).toBe("help");
    expect((await runClientCommand(app, s, "chat", "mcp", ""))?.kind).toBe("connectors");
    expect(s.model.messages.length).toBe(before);
  });

  it("remembers recently used commands in UI state", async () => {
    const app = await start();
    app.setUi({ recentCommands: pushRecent(app.ui.recentCommands, "review") });
    expect(rankCommands((await app.client.command.list({ kind: "chat" })).commands, "", app.ui.recentCommands)[0].name).toBe("review");
  });
});
