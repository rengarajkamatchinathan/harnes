// Model race: the pure helpers, and whole races against the in-memory engine (send, switch
// model + regenerate per extra model, restore, keep an answer = switch branch).
import { describe, expect, it } from "vitest";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { answerUsage, branchBase, cancelRace, keepAnswer, promptOf, RaceBook, raceModels, raceProgress, startRace, toggleExtra, waitTurn, type Race } from "../src/state/race";
import { emptyModel, type MessageView } from "../src/state/sessionModel";

const until = async (f: () => boolean, ms = 5000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

const msg = (id: string, role: MessageView["role"], parentId: string | null, extra: Partial<MessageView> = {}): MessageView => ({ id, role, parentId, turnId: null, model: null, createdAt: "", parts: [], rev: 0, ...extra });

describe("race helpers", () => {
  it("races the current model first, then usable distinct extras, at most three", () => {
    expect(raceModels("a", ["b", "a", "x", "c", "d"], ["a", "b", "c", "d"])).toEqual(["a", "b", "c"]);
    expect(raceModels("a", ["x"], ["a"])).toEqual(["a"]);
  });

  it("toggles extras, never the current model, at most two", () => {
    expect(toggleExtra([], "b", "a")).toEqual(["b"]);
    expect(toggleExtra(["b"], "b", "a")).toEqual([]);
    expect(toggleExtra(["b"], "a", "a")).toEqual(["b"]);
    expect(toggleExtra(["b", "c"], "d", "a")).toEqual(["b", "c"]);
  });

  it("reports progress", () => {
    const a = (status: Race["answers"][number]["status"]) => ({ model: "m", status, turnId: null, messages: [] });
    expect(raceProgress({ answers: [a("done"), a("running"), a("waiting")] })).toEqual({ running: 2, settled: 1, total: 3 });
    expect(raceProgress({ answers: [a("done"), a("failed")] })).toEqual({ running: null, settled: 2, total: 2 });
  });

  it("prefers the turn's live usage, else sums the saved messages'", () => {
    const live = { input: 10, output: 5, reasoning: 0, cacheRead: 0, usd: 0, steps: 1, ms: 900 };
    expect(answerUsage(live, [])).toBe(live);
    const u = { input: 100, output: 20, reasoning: 0, cacheRead: 0, usd: 0.01, steps: 1 };
    expect(answerUsage({ ...live, steps: 0 }, [msg("a1", "assistant", "u", { usage: u }), msg("a2", "assistant", "a1", { usage: u })])).toMatchObject({ input: 200, output: 40, usd: 0.02, steps: 2, ms: 900 });
    expect(answerUsage(undefined, [])).toBeUndefined();
  });

  it("branches from the newest captured assistant reply and finds a reply's prompt", () => {
    const answers = [
      { model: "a", status: "done" as const, turnId: "t1", messages: [msg("a1", "assistant", "u")] },
      { model: "b", status: "failed" as const, turnId: "t2", messages: [] },
    ];
    expect(branchBase({ answers })).toBe("a1");
    expect(branchBase({ answers: [answers[1]] })).toBeNull();
    const m = { messages: [msg("u", "user", null), msg("a1", "assistant", "u"), msg("a2", "assistant", "a1")] };
    expect(promptOf(m, m.messages[2])).toBe("u");
  });

  it("gives up waiting when no turn starts", async () => {
    const model = emptyModel("s");
    const r = await waitTurn({ model, subscribe: () => () => {} }, new Set(), () => {}, 10);
    expect(r).toEqual({ turnId: null, status: "failed", error: null });
  });
});

async function setup(tickMs = 0) {
  const platform = createMockPlatform({ tickMs });
  const app = new App(platform);
  await app.start();
  await until(() => app.models.length > 0);
  const id = await app.createSession({ kind: "chat" });
  const s = app.session(id);
  await until(() => !s.loading);
  const setModel = (model: string) => app.client.session.update({ session_id: id, model });
  await setModel("mock/genie-small");
  await until(() => s.model.info?.model === "mock/genie-small");
  const deps = { setModel, describeError: (e: unknown) => app.describeError(e) };
  return { platform, app, id, s, deps, book: new RaceBook() };
}

describe("a race over the mock engine", () => {
  it("answers with each model as sibling branches, restores the model, and keeps the chosen answer", async () => {
    const { platform, id, s, deps, book } = await setup();
    const race = await startRace(s, [{ type: "text", text: "how do I double numbers?" }], ["mock/genie-small", "mock/genie-large", "mock/genie-test"], deps, book);
    expect(book.shown(id)).toBe(race);
    await until(() => !race.running);
    expect(race.answers.map((a) => a.status)).toEqual(["done", "done", "done"]);
    const text = (i: number) => race.answers[i].messages.flatMap((m) => m.parts).map((p) => p.text).join("");
    expect(text(0)).toContain("quick overview");
    expect(text(1)).toContain("more depth");
    expect(text(2)).not.toBe(text(0));
    expect(race.answers[1].usage).toMatchObject({ input: 1400, output: 620, steps: 1 });
    expect(race.answers[1].usage?.ms).toBeGreaterThanOrEqual(0);
    // The session model is back to the one the user picked.
    expect(platform.sessions.get(id)!.info.model).toBe("mock/genie-small");
    // All three are siblings; the last one is the active branch.
    const leaves = race.answers.map((a) => a.messages[a.messages.length - 1].id);
    await until(() => s.model.messages.some((m) => m.id === leaves[2]));
    expect(s.model.siblings[leaves[2]]).toEqual(leaves);
    expect(book.forPrompt(id, race.userMessageId)).toBe(race);

    expect(await keepAnswer(s, race, 0, book)).toBe(true);
    expect(race.kept).toBe(0);
    expect(s.model.messages.map((m) => m.id)).toEqual([race.userMessageId, leaves[0]]);
    // The next prompt continues from the kept answer.
    await s.send([{ type: "text", text: "thanks" }]);
    const sent = platform.sessions.get(id)!.messages.find((m) => m.role === "user" && m.parts.some((p) => p.type === "text" && p.text === "thanks"));
    expect(sent?.parent_id).toBe(leaves[0]);
  });

  it("shows a failed model's error while the others still answer", async () => {
    const { s, deps, book } = await setup();
    // Genie Test fails on "error"; Genie Small answers normally.
    const race = await startRace(s, [{ type: "text", text: "error" }], ["mock/genie-small", "mock/genie-test"], deps, book);
    await until(() => !race.running);
    expect(race.answers[0].status).toBe("done");
    expect(race.answers[1].status).toBe("failed");
    expect(race.answers[1].error).toBeTruthy();
  });

  it("stops: the running answer is cancelled and the rest never start", async () => {
    const { platform, id, s, deps, book } = await setup(5);
    const race = await startRace(s, [{ type: "text", text: "hello" }], ["mock/genie-small", "mock/genie-large", "mock/genie-test"], deps, book);
    await until(() => race.answers[0].status === "running");
    cancelRace(s, race, book);
    await until(() => !race.running);
    expect(race.answers.map((a) => a.status)).toEqual(["cancelled", "cancelled", "cancelled"]);
    expect(platform.sessions.get(id)!.info.model).toBe("mock/genie-small");
  });
});
