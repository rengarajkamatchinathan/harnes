// Terminal tab (05 §10, D55) over the mock engine: open in a Code session, input/output,
// reattach from an offset after a reconnect (no duplicates), a new view replays, a view
// whose offset fell out of the ring gets reset + checkpoint, exit state, close, limits.
import { describe, expect, it } from "vitest";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { MAX_TERMINALS, b64ToBytes, bytesToB64, type TermSink } from "../src/state/terminals";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

class Screen implements TermSink {
  text = "";
  resets = 0;
  reset() {
    this.resets++;
    this.text = "";
  }
  write(bytes: Uint8Array) {
    this.text += new TextDecoder().decode(bytes);
  }
}

async function setup(opts: { terminalRing?: number } = {}) {
  const platform = createMockPlatform({ tickMs: 0, ...opts });
  const app = new App(platform);
  await app.start();
  await until(() => app.models.length > 0);
  const sid = await app.createSession({ kind: "code", workspacePath: "/tmp/repo" });
  return { platform, app, sid };
}

describe("terminals", () => {
  it("base64 helpers round-trip bytes", () => {
    const bytes = Uint8Array.from([0, 1, 127, 128, 255, 27, 91]);
    expect([...b64ToBytes(bytesToB64(bytes))]).toEqual([...bytes]);
  });

  it("opens, streams input/output and reattaches from the offset after a reconnect", async () => {
    const { platform, app, sid } = await setup();
    const info = await app.terminals.open(sid, 80, 24);
    expect(app.terminals.list(sid).map((x) => x.id)).toEqual([info.id]);
    expect(app.ui.terminals[sid]?.[0]?.id).toBe(info.id); // persisted for reload

    const screen = new Screen();
    app.terminals.attach(info.id, screen);
    await until(() => screen.text === "mock$ ");
    await app.terminals.input(info.id, "echo hi\r");
    await until(() => screen.text.includes("hi\r\nmock$ "));
    const before = screen.text;

    // Simulated reconnect: the relay is ready again; live views re-attach from their offset.
    platform.emit({ kind: "status", state: "reconnecting" });
    platform.emit({ kind: "status", state: "ready" });
    await app.terminals.input(info.id, "echo again\r");
    await until(() => screen.text.includes("again\r\nmock$ "));
    expect(screen.text.startsWith(before)).toBe(true);
    expect(screen.text.split("echo hi").length).toBe(2); // no duplicated replay
    expect(screen.resets).toBe(0);
  });

  it("a new view (tab switch / reload) replays the ring", async () => {
    const { app, sid } = await setup();
    const info = await app.terminals.open(sid, 80, 24);
    const a = new Screen();
    const detach = app.terminals.attach(info.id, a);
    await until(() => a.text.length > 0);
    await app.terminals.input(info.id, "echo one\r");
    await until(() => a.text.includes("one\r\n"));
    detach();

    const b = new Screen();
    app.terminals.attach(info.id, b);
    await until(() => b.text === a.text);
  });

  it("a truncated reattach resets the view and writes the checkpoint", async () => {
    const { app, sid } = await setup({ terminalRing: 32 });
    const info = await app.terminals.open(sid, 80, 24);
    const screen = new Screen();
    app.terminals.attach(info.id, screen);
    await until(() => screen.text.length > 0);
    await app.terminals.input(info.id, "echo " + "x".repeat(60) + "\r");
    await until(() => screen.text.endsWith("mock$ "));
    // Force a gap: the view's offset is older than the ring now.
    (app.terminals as unknown as { views: Map<string, { offset: number }> }).views.get(info.id)!.offset = 1;
    app.terminals.reattachAll();
    await until(() => screen.resets === 1);
    expect(screen.text).toBe("mock$ ");
  });

  it("marks exit, keeps the scrollback readable and frees the slot", async () => {
    const { app, sid } = await setup();
    const info = await app.terminals.open(sid, 80, 24);
    const screen = new Screen();
    const detach = app.terminals.attach(info.id, screen);
    await until(() => screen.text.length > 0);
    await app.terminals.input(info.id, "exit 3\r");
    await until(() => !!app.terminals.get(info.id)?.exited);
    expect(app.terminals.get(info.id)?.code).toBe(3);
    expect(app.ui.terminals[sid] ?? []).toEqual([]);
    detach();
    const again = new Screen();
    app.terminals.attach(info.id, again);
    expect(again.text).toContain("exit");
    await app.terminals.close(info.id);
    expect(app.terminals.list(sid)).toEqual([]);
  });

  it("closes, limits to 8 per session and refuses Chat sessions", async () => {
    const { app, sid } = await setup();
    const ids = [];
    for (let i = 0; i < MAX_TERMINALS; i++) ids.push((await app.terminals.open(sid, 80, 24)).id);
    await expect(app.terminals.open(sid, 80, 24)).rejects.toThrow(/At most 8/);
    await app.terminals.close(ids[0]);
    await until(() => app.terminals.list(sid).length === MAX_TERMINALS - 1);
    await app.terminals.open(sid, 80, 24);

    const chat = await app.createSession({ kind: "chat" });
    await expect(app.terminals.open(chat, 80, 24)).rejects.toMatchObject({ data: { kind: "invalid_state" } });
  });

  it("a terminal the engine no longer knows shows as ended after a reload", async () => {
    const platform = createMockPlatform({ tickMs: 0 });
    await platform.saveState({ firstRunDone: true, terminals: { ses_gone: [{ id: "term_gone", title: "Terminal 1" }] } });
    const app = new App(platform);
    await app.start();
    expect(app.terminals.list("ses_gone").length).toBe(1);
    app.terminals.attach("term_gone", new Screen());
    await until(() => !!app.terminals.get("term_gone")?.lost);
  });
});
