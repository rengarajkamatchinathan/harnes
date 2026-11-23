import { describe, expect, it, vi } from "vitest";
import { BudgetSpend, budgetState, checksFor, compactTokens, daysLeftInMonth, formatAmount, monthKey, monthQuery, monthStart, normalizeBudgets, parseAmount, percentUsed, sendGate, spendFromRows, type Budget, type Budgets } from "../src/state/budget";

const rows = [
  { keys: ["prj_a"], usage: { input_tokens: 600_000, output_tokens: 220_000 }, cost: { usd: 0 } },
  { keys: [""], usage: { input_tokens: 50_000, output_tokens: 10_000 }, cost: { usd: 1.5 } },
  { keys: ["prj_b"], usage: { input_tokens: 1000 }, cost: { usd: 0.25 } },
];
const tokens = (amount: number, action: Budget["action"] = "ask", fallbackModel?: string): Budget => ({ unit: "tokens", amount, action, fallbackModel });

describe("budget guard: month window", () => {
  it("starts on the local 1st and counts days left including today", () => {
    const now = new Date(2026, 10, 20, 15, 30);
    expect(monthStart(now)).toEqual(new Date(2026, 10, 1));
    expect(monthKey(now)).toBe("2026-11");
    expect(daysLeftInMonth(now)).toBe(11);
    expect(daysLeftInMonth(new Date(2026, 10, 30))).toBe(1);
    expect(daysLeftInMonth(new Date(2028, 1, 1))).toBe(29); // leap year
  });
  it("queries from the 1st, grouped by project, padding `to` by a day", () => {
    expect(monthQuery(new Date(2026, 11, 31, 12))).toEqual({ from: "2026-12-01", to: "2027-01-01", group_by: ["project"] });
  });
});

describe("budget guard: spend and state", () => {
  it("sums tokens and cost per project and in total", () => {
    const s = spendFromRows(rows);
    expect(s.byProject.prj_a).toEqual({ tokens: 820_000, usd: 0 });
    expect(s.byProject[""]).toEqual({ tokens: 60_000, usd: 1.5 });
    expect(s.total).toEqual({ tokens: 881_000, usd: 1.75 });
  });
  it("computes percent used per unit and the ok / warn / over thresholds", () => {
    expect(percentUsed(tokens(1_000_000), { tokens: 820_000, usd: 0 })).toBeCloseTo(0.82);
    expect(percentUsed({ unit: "usd", amount: 2 }, { tokens: 9e9, usd: 1 })).toBe(0.5);
    expect(percentUsed({ unit: "usd", amount: 0 }, { tokens: 1, usd: 1 })).toBe(0);
    expect(budgetState(0.79)).toBe("ok");
    expect(budgetState(0.8)).toBe("warn");
    expect(budgetState(1)).toBe("over");
    expect(budgetState(1.4)).toBe("over");
  });
  it("lists the project and overall checks for a chat, most used first", () => {
    const b: Budgets = { overall: { unit: "usd", amount: 10, action: "warn" }, projects: { prj_a: tokens(1_000_000) } };
    const s = spendFromRows(rows);
    const c = checksFor(b, s, "prj_a", "Demo");
    expect(c.map((x) => [x.scope, x.state])).toEqual([["project", "warn"], ["overall", "ok"]]);
    expect(c[0].name).toBe("Demo");
    expect(checksFor(b, s, null).map((x) => x.scope)).toEqual(["overall"]);
    expect(checksFor({ overall: null, projects: {} }, s, "prj_a")).toEqual([]);
    // A project without a row yet has spent nothing.
    expect(checksFor({ overall: null, projects: { prj_new: tokens(10) } }, s, "prj_new")[0].used).toBe(0);
  });
});

describe("budget guard: send gate", () => {
  const s = { byProject: { p: { tokens: 1_200_000, usd: 0 } }, total: { tokens: 1_200_000, usd: 0 } };
  const gate = (b: Budget, model = "big", usable?: (m: string) => boolean) => sendGate(checksFor({ overall: null, projects: { p: b } }, s, "p"), model, usable);
  it("only acts at 100%", () => {
    expect(sendGate(checksFor({ overall: null, projects: { p: tokens(2_000_000) } }, s, "p"), "big").kind).toBe("send");
  });
  it("warn only sends, ask asks, switch moves to the cheaper model", () => {
    expect(gate(tokens(1_000_000, "warn")).kind).toBe("send");
    expect(gate(tokens(1_000_000, "ask")).kind).toBe("ask");
    expect(gate(tokens(1_000_000, "switch", "small"))).toMatchObject({ kind: "switch", model: "small" });
  });
  it("doesn't switch to the model already in use, a missing one, or one that can't be used", () => {
    expect(gate(tokens(1_000_000, "switch", "small"), "small").kind).toBe("send");
    expect(gate(tokens(1_000_000, "switch")).kind).toBe("send");
    expect(gate(tokens(1_000_000, "switch", "small"), "big", () => false).kind).toBe("send");
  });
  it("ask wins over switch when two budgets are over", () => {
    const c = checksFor({ overall: tokens(1, "ask"), projects: { p: tokens(1, "switch", "small") } }, s, "p");
    expect(sendGate(c, "big").kind).toBe("ask");
  });
});

describe("budget guard: amounts and stored state", () => {
  it("parses and formats amounts", () => {
    expect(parseAmount("1M")).toBe(1_000_000);
    expect(parseAmount("250k")).toBe(250_000);
    expect(parseAmount("1,500,000")).toBe(1_500_000);
    expect(parseAmount("$49.5")).toBe(49.5);
    expect(parseAmount("0")).toBeNull();
    expect(parseAmount("abc")).toBeNull();
    expect(compactTokens(1_000_000)).toBe("1M");
    expect(compactTokens(1_250_000)).toBe("1.25M");
    expect(compactTokens(820_000)).toBe("820k");
    expect(formatAmount("usd", 50)).toBe("$50.00");
  });
  it("drops invalid stored budgets", () => {
    expect(normalizeBudgets(undefined)).toEqual({ overall: null, projects: {} });
    const n = normalizeBudgets({ overall: { unit: "usd", amount: -1, action: "ask" }, projects: { a: tokens(5), b: { unit: "x", amount: 1, action: "ask" } } });
    expect(n).toEqual({ overall: null, projects: { a: tokens(5) } });
  });
});

describe("budget guard: spend loader", () => {
  it("loads month-to-date spend, coalescing overlapping loads", async () => {
    let calls = 0;
    const query = vi.fn(async () => (calls++, { rows }));
    const sp = new BudgetSpend(query);
    await Promise.all([sp.load(), sp.load()]);
    expect(sp.total.tokens).toBe(881_000);
    expect(sp.loadedAt).toBeGreaterThan(0);
    // The second call queued exactly one follow-up load.
    await new Promise((r) => setTimeout(r, 0));
    expect(calls).toBe(2);
  });
  it("keeps the last error and skips refreshes while fresh", async () => {
    const sp = new BudgetSpend(async () => {
      throw new Error("down");
    });
    await sp.load();
    expect(sp.error).toBe("down");
    vi.useFakeTimers();
    const ok = new BudgetSpend(vi.fn(async () => ({ rows: [] })));
    ok.loadedAt = Date.now();
    ok.month = monthKey();
    const spy = vi.spyOn(ok, "load");
    ok.refresh(0, 60_000);
    vi.runAllTimers();
    expect(spy).not.toHaveBeenCalled();
    ok.refresh(10);
    vi.runAllTimers();
    expect(spy).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});

describe("budget guard over the mock engine", () => {
  it("reports Demo project spend, warns at 82% and counts new replies", async () => {
    const { createMockPlatform } = await import("../src/platform/mock");
    const { App } = await import("../src/state/app");
    const { AppContext } = await import("../src/ui/context");
    const { BudgetNotice, ProjectBudgetCard } = await import("../src/ui/BudgetGuard");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const { createElement } = await import("react");
    const app = new App(createMockPlatform({ tickMs: 0, reply: () => "ok" }));
    await app.start();
    app.setUi({ budgets: { overall: null, projects: { prj_demo: tokens(1_000_000, "ask") } } });
    await app.budgetSpend.load();
    const checks = checksFor(app.ui.budgets, app.budgetSpend, "prj_demo", "Demo project");
    expect(checks[0]).toMatchObject({ state: "warn", used: 815_000 });
    const wrap = (el: ReturnType<typeof createElement>) => renderToStaticMarkup(createElement(AppContext.Provider, { value: app }, el));
    expect(wrap(createElement(BudgetNotice, { checks, projectId: "prj_demo" }))).toContain("Demo project has used 81% of its 1M-token monthly budget.");
    const card = wrap(createElement(ProjectBudgetCard, { projectId: "prj_demo" }));
    expect(card).toContain("815k of 1M");
    expect(card).toMatch(/days? left this month/);

    const id = await app.createSession({ kind: "chat", projectId: "prj_demo" });
    const s = app.session(id);
    await new Promise((r) => setTimeout(r, 20));
    await s.send([{ type: "text", text: "hi" }]);
    const end = Date.now() + 3000;
    while (s.model.status !== "idle" || !s.model.messages.some((m) => m.role === "assistant")) {
      if (Date.now() > end) throw new Error("timed out");
      await new Promise((r) => setTimeout(r, 5));
    }
    await app.budgetSpend.load();
    expect(app.budgetSpend.byProject.prj_demo.tokens).toBeGreaterThan(815_000);
  });
});
