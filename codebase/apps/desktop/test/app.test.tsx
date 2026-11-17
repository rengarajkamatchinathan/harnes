// End to end through the Platform adapter with the in-memory engine: connect, workspace
// feed, create a chat, stream a reply, answer a permission ask relayed as a server
// request, resume after a simulated reload.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { AppContext } from "../src/ui/context";
import { AskCard } from "../src/ui/chat/AskCard";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

describe("app over the mock platform", () => {
  it("creates a chat and streams a reply", async () => {
    const platform = createMockPlatform({ tickMs: 0, reply: () => "# Hello\n\nworld" });
    const app = new App(platform);
    await app.start();
    expect(app.status.state).toBe("ready");
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "hi" }]);
    await until(() => s.model.status === "idle" && s.model.messages.some((m) => m.role === "assistant" && m.parts.some((p) => !p.streaming && p.text.includes("world"))));
    expect(s.model.usage?.context_window).toBe(200000);
    await until(() => app.sessions.get(id)?.title === "hi");
    expect(app.ui.activeSession).toBe(id);
  });

  it("relays a permission ask and answers it as the server request's reply", async () => {
    const platform = createMockPlatform({ tickMs: 0, reply: () => "done" });
    const app = new App(platform);
    await app.start();
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "please run ls" }]);
    await until(() => s.model.asks.size > 0);
    const ask = [...s.model.asks.values()][0];
    expect(ask.tool).toBe("shell");

    const html = renderToStaticMarkup(
      <AppContext.Provider value={app}>
        <AskCard ask={ask} onAnswer={async () => {}} />
      </AppContext.Provider>,
    );
    expect(html).toContain("Allow once");
    expect(html).toContain("ls -la");

    await s.answer(ask, { decision: "once" });
    await until(() => s.model.status === "idle" && s.model.asks.size === 0);
    const tool = [...s.model.tools.values()][0];
    expect(tool.outcome).toBe("ok");
  });

  it("re-opens a session from its snapshot (webview reload / resync)", async () => {
    const platform = createMockPlatform({ tickMs: 0, reply: () => "again" });
    const app = new App(platform);
    await app.start();
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "x" }]);
    await until(() => s.model.status === "idle" && s.model.messages.length >= 2);
    await s.reload();
    expect(s.model.messages.map((m) => m.role)).toEqual(["user", "assistant"]);
  });

  it("persists UI state through the platform", async () => {
    const platform = createMockPlatform({ tickMs: 0 });
    const app = new App(platform);
    await app.start();
    app.setUi({ panelWidth: 500 });
    await new Promise((r) => setTimeout(r, 350));
    expect((await platform.loadState()).panelWidth).toBe(500);
    expect(app.openDeepLink("genie://settings/providers")).toBe(true);
    expect(app.ui).toMatchObject({ view: "settings", settingsSection: "providers" });
  });
});
