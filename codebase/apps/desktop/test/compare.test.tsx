// The comparison view of a finished race, rendered statically: one card per model with its
// answer, stats, error, and "Keep this".
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import type { Race } from "../src/state/race";
import { AppContext } from "../src/ui/context";
import { CompareChip, CompareView } from "../src/ui/chat/Compare";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

describe("compare view", () => {
  it("renders each answer with its model, stats and keep button", async () => {
    const app = new App(createMockPlatform({ tickMs: 0 }));
    await app.start();
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "chat" });
    const s = app.session(id);
    const answer = (model: string, text: string) => ({
      model,
      status: "done" as const,
      turnId: "t",
      messages: [{ id: `m_${model}`, role: "assistant" as const, parentId: "u", turnId: "t", model, createdAt: "", rev: 0, parts: [{ id: `p_${model}`, kind: "text" as const, text, json: "", final: null, streaming: false }] }],
      usage: { input: 1200, output: 300, reasoning: 0, cacheRead: 0, usd: 0.0021, steps: 1, ms: 2400 },
    });
    const race: Race = {
      id: "r",
      sessionId: id,
      userMessageId: "u",
      answers: [answer("mock/genie-small", "Small **answer**"), answer("mock/genie-large", "Large answer"), { model: "mock/genie-test", status: "failed", turnId: null, messages: [], error: "boom" }],
      running: false,
      cancelled: false,
      kept: null,
    };
    const html = renderToStaticMarkup(
      <AppContext.Provider value={app}>
        <CompareView store={s} race={race} />
      </AppContext.Provider>,
    );
    expect(html).toContain("Comparing 3 answers");
    expect(html).toContain("Genie Small");
    expect(html).toContain("Genie Large");
    expect(html).toContain("<strong>answer</strong>");
    expect(html).toContain("boom");
    expect(html).toContain("2.4s");
    expect(html).toContain("$0.0021");
    // The failed card has no "Keep this" at all; the two answers do.
    expect(html.match(/Keep this/g)).toHaveLength(2);
    expect(html.match(/disabled=""[^>]*>Keep this/g)).toBeNull();
  });

  it("shows the composer chip only for a real race", () => {
    expect(renderToStaticMarkup(<CompareChip models={["a"]} />)).toBe("");
    expect(renderToStaticMarkup(<CompareChip models={["a", "b", "c"]} />)).toContain("Comparing 3 models");
    // The chip names the models that will answer.
    const named = renderToStaticMarkup(<CompareChip models={["az/gpt", "g/flash"]} nameOf={(id) => ({ "az/gpt": "GPT-5.2", "g/flash": "Gemini Flash" })[id] ?? id} />);
    expect(named).toContain("GPT-5.2");
    expect(named).toContain("Gemini Flash");
    expect(named).toContain("vs");
  });
});
