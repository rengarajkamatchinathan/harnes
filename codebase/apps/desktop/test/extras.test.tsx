// UI pieces that need no new protocol (05 §2.2, §2.3, §4): MCP resource/prompt pickers,
// OS notifications for paused asks, Install in PATH, licence notices, native pickers.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { AppContext } from "../src/ui/context";
import { missingArgs, parsePrompts, parseResources, promptCommand } from "../src/ui/chat/McpPickers";
import { UsageChip } from "../src/ui/chat/Transcript";
import { AccountSection, InstallCli } from "../src/ui/settings/Sections";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

async function started(opts = {}) {
  const platform = createMockPlatform({ tickMs: 0, ...opts });
  const app = new App(platform);
  await app.start();
  await until(() => app.models.length > 0);
  return { platform, app };
}

describe("MCP pickers", () => {
  it("parse resources (templates skipped) and prompts with arguments", async () => {
    const { app } = await started();
    const res = parseResources("files", (await app.client.mcp.resources({ name: "files" })).items);
    expect(res).toEqual([{ server: "files", uri: "file:///notes/todo.md", name: "todo.md", description: undefined, mime: "text/markdown" }]);
    const prompts = parsePrompts("files", (await app.client.mcp.prompts({ name: "files" })).items);
    expect(prompts[0].arguments).toEqual([
      { name: "path", description: undefined, required: true },
      { name: "style", description: undefined, required: false },
    ]);
  });

  it("build the /server:prompt command and require required arguments", () => {
    const p = { server: "files", name: "summarize", arguments: [{ name: "path", required: true }, { name: "style", required: false }] };
    expect(missingArgs(p, {})).toEqual(["path"]);
    expect(missingArgs(p, { path: "a" })).toEqual([]);
    expect(promptCommand(p, { path: 'my "notes".md', style: "" })).toBe('/files:summarize path="my \\"notes\\".md"');
  });

  it("a resource read becomes a blob attachment", async () => {
    const { app } = await started();
    const r = await app.client.mcp.resource.read({ name: "files", uri: "file:///notes/todo.md" });
    expect(r.blob_id).toMatch(/^blob_/);
    expect(r.mime).toBe("text/markdown");
  });
});

describe("OS notifications", () => {
  it("notify on a paused ask only when the window is in the background", async () => {
    const { platform, app } = await started({ reply: () => "ok" });
    app.isForeground = () => true;
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "run ls" }]);
    await until(() => s.model.asks.size > 0);
    expect(platform.notifications).toEqual([]);
    await s.answer([...s.model.asks.values()][0], { decision: "once" });
    await until(() => s.model.status === "idle");

    app.isForeground = () => false;
    await s.send([{ type: "text", text: "run ls again" }]);
    await until(() => platform.notifications.length > 0);
    expect(platform.notifications[0].title).toBe("Genie needs your approval");
    expect(platform.notifications[0].body).toContain("shell");

    // The workspace `notification` too, and nothing when turned off.
    app.setUi({ osNotifications: false });
    platform.emit({ kind: "batch", messages: [{ method: "notification", params: { title: "Done", body: "x" } }] });
    expect(platform.notifications.length).toBe(1);
  });
});

describe("settings extras", () => {
  it("Install in PATH, notices and pickers go through the platform", async () => {
    const { platform, app } = await started();
    expect((await platform.installCli()).path).toContain("genie");
    expect(await platform.notices()).toContain("OpenCode");
    expect(await platform.pickFolder()).toBe("/tmp/mock-repo");
    const html = renderToStaticMarkup(
      <AppContext.Provider value={app}>
        <InstallCli />
        <AccountSection />
      </AppContext.Provider>,
    );
    expect(html).toContain('Install &quot;genie&quot; command in PATH');
    expect(html).toContain("Third-party licences");
  });

  it("per-message usage chip", () => {
    const html = renderToStaticMarkup(<UsageChip u={{ input: 1200, output: 300, reasoning: 0, cacheRead: 0, usd: 0.0021, steps: 1 }} />);
    expect(html).toContain("1.5k tokens · $0.0021");
  });
});
