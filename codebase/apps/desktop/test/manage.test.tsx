// Engine features wired into the UI (02 additions): move a chat to/out of a project,
// trusted folders list/revoke, saved permission rules list/remove (locked → managed),
// delete all data (two steps), project.file.list, per-chat web_search/code_execution
// toggles, per-message usage from session.get.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { AppContext } from "../src/ui/context";
import { PermissionsSection } from "../src/ui/settings/ManageSections";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

async function started() {
  const platform = createMockPlatform({ tickMs: 0, reply: () => "hello" });
  const app = new App(platform);
  await app.start();
  await until(() => app.models.length > 0 && app.projects.length > 0);
  return { platform, app };
}

describe("engine features in the UI", () => {
  it("moves a chat into a project and out again; Code sessions are refused", async () => {
    const { app } = await started();
    const id = await app.createSession({ kind: "chat" });
    await app.moveToProject(id, "prj_demo");
    expect(app.sessions.get(id)?.project_id).toBe("prj_demo");
    await app.moveToProject(id, null);
    expect(app.sessions.get(id)?.project_id).toBeNull();
    const code = await app.createSession({ kind: "code", workspacePath: "/tmp/repo" });
    await expect(app.moveToProject(code, "prj_demo")).rejects.toMatchObject({ data: { kind: "invalid_params" } });
  });

  it("lists and revokes trusted folders", async () => {
    const { app } = await started();
    expect((await app.client.trust.list({})).items.map((f) => f.path)).toEqual(["/tmp/mock-repo"]);
    await app.client.trust.set({ path: "/tmp/mock-repo", decision: "revoke" });
    expect((await app.client.trust.list({})).items).toEqual([]);
  });

  it("lists saved rules, removes one and refuses a locked one", async () => {
    const { app } = await started();
    const { rules } = await app.client.permissions.rules.list({});
    expect(rules.map((r) => r.rule)).toEqual(["Bash(ls:*)", "WebFetch(domain:evil.example)"]);
    await app.client.permissions.rules.remove({ rule: "Bash(ls:*)", class: "allow", scope: "user" });
    await expect(app.client.permissions.rules.remove({ rule: "WebFetch(domain:evil.example)", class: "deny", scope: "managed" })).rejects.toMatchObject({ data: { kind: "locked" } });
    const html = renderToStaticMarkup(
      <AppContext.Provider value={app}>
        <PermissionsSection />
      </AppContext.Provider>,
    );
    expect(html).toContain("Trusted folders");
  });

  it("deletes all data in two steps and reloads from a fresh snapshot", async () => {
    const { app } = await started();
    const id = await app.createSession({ kind: "chat" });
    const plan = await app.client.data.delete_all.begin({});
    expect(plan.sessions).toBe(1);
    await expect(app.client.data.delete_all.confirm({ token: "wrong" })).rejects.toBeTruthy();
    const r = await app.client.data.delete_all.confirm({ token: plan.token });
    expect(r.sessions_deleted).toBe(1);
    await app.reloadAfterDeleteAll();
    await until(() => app.sessions.size === 0 && app.projects.length === 0);
    expect(app.sessions.has(id)).toBe(false);
    expect(app.ui.activeSession).toBeNull();
    // A used token can't be replayed.
    await expect(app.client.data.delete_all.confirm({ token: plan.token })).rejects.toBeTruthy();
  });

  it("lists project files", async () => {
    const { app } = await started();
    await app.client.project.file.add({ project_id: "prj_demo", blob_id: "blob_x", name: "spec.md" });
    const { files } = await app.client.project.file.list({ project_id: "prj_demo" });
    expect(files.map((f) => f.name)).toEqual(["spec.md"]);
    expect(files[0].tokens).toBe(25);
  });

  it("toggles web search and code execution per chat", async () => {
    const { app } = await started();
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading && !!s.model.info);
    expect(s.model.info?.web_search).toBe(true);
    expect(s.model.info?.code_execution).toBe(false);
    await s.setToggle("code_execution", true);
    await s.setToggle("web_search", false);
    expect(s.model.info?.code_execution).toBe(true);
    expect(s.model.info?.web_search).toBe(false);
    await s.reload();
    expect(s.model.info?.code_execution).toBe(true);
  });

  it("shows per-message usage from session.get", async () => {
    const { app } = await started();
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "hi" }]);
    await until(() => s.model.status === "idle" && s.model.messages.some((m) => m.role === "assistant"));
    await s.reload();
    const reply = s.model.messages.find((m) => m.role === "assistant")!;
    expect(reply.usage).toMatchObject({ input: 1200, output: 300, usd: 0.0021 });
  });
});
