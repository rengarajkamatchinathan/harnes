// Budget guard: per-project and overall monthly limits (tokens or USD), kept desktop-side in
// UiState. Month-to-date spend comes from `usage.query` grouped by Chat project. The maths
// here is pure (for tests); `BudgetSpend` is the small cached, debounced spend loader.
import type { UsageQueryParams, UsageQueryResult, UsageRow } from "@genie/protocol";
import { Emitter } from "./emitter";

export type BudgetUnit = "tokens" | "usd";
/** What happens when a send would go out at or over 100%. */
export type BudgetAction = "warn" | "ask" | "switch";
export type BudgetState = "ok" | "warn" | "over";

export interface Budget {
  unit: BudgetUnit;
  /** Monthly limit, in tokens or US dollars; > 0. */
  amount: number;
  action: BudgetAction;
  /** For "switch": the cheaper model the chat moves to. */
  fallbackModel?: string | null;
}

export interface Budgets {
  /** Across every chat and code session. */
  overall: Budget | null;
  /** Per Chat project id. */
  projects: Record<string, Budget>;
}

export const NO_BUDGETS: Budgets = { overall: null, projects: {} };

export interface Spend {
  tokens: number;
  usd: number;
}
export const ZERO_SPEND: Spend = { tokens: 0, usd: 0 };

export const WARN_AT = 0.8;

const pad = (n: number) => String(n).padStart(2, "0");
/** Local calendar date as `YYYY-MM-DD`. */
export const ymd = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

/** Local midnight on the 1st of `now`'s month. */
export function monthStart(now: Date = new Date()): Date {
  return new Date(now.getFullYear(), now.getMonth(), 1);
}

/** `YYYY-MM`: budgets reset when this changes. */
export const monthKey = (now: Date = new Date()) => `${now.getFullYear()}-${pad(now.getMonth() + 1)}`;

/** Days left in the month, counting today (30 Nov → 1). */
export function daysLeftInMonth(now: Date = new Date()): number {
  const last = new Date(now.getFullYear(), now.getMonth() + 1, 0).getDate();
  return last - now.getDate() + 1;
}

/** The `usage.query` window for month-to-date spend (the engine compares by day; `to` is padded a day for time zones). */
export function monthQuery(now: Date = new Date()): UsageQueryParams {
  return { from: ymd(monthStart(now)), to: ymd(new Date(now.getTime() + 86_400_000)), group_by: ["project"] };
}

export const tokensOfRow = (r: Pick<UsageRow, "usage">) => (r.usage.input_tokens ?? 0) + (r.usage.output_tokens ?? 0);

/** Rows grouped by project → spend per project id ("" = no project) and the total. */
export function spendFromRows(rows: UsageRow[]): { byProject: Record<string, Spend>; total: Spend } {
  const byProject: Record<string, Spend> = {};
  let total = ZERO_SPEND;
  for (const r of rows) {
    const s = { tokens: tokensOfRow(r), usd: r.cost?.usd ?? 0 };
    const k = r.keys[0] ?? "";
    const prev = byProject[k] ?? ZERO_SPEND;
    byProject[k] = { tokens: prev.tokens + s.tokens, usd: prev.usd + s.usd };
    total = { tokens: total.tokens + s.tokens, usd: total.usd + s.usd };
  }
  return { byProject, total };
}

export const usedOf = (b: Pick<Budget, "unit">, s: Spend) => (b.unit === "usd" ? s.usd : s.tokens);

/** Fraction of the limit used (1 = 100%); 0 for an empty limit. */
export function percentUsed(b: Pick<Budget, "unit" | "amount">, s: Spend): number {
  if (!(b.amount > 0)) return 0;
  return usedOf(b, s) / b.amount;
}

export function budgetState(fraction: number): BudgetState {
  if (fraction >= 1) return "over";
  if (fraction >= WARN_AT) return "warn";
  return "ok";
}

export function validBudget(b: unknown): b is Budget {
  const x = b as Budget | null;
  return !!x && (x.unit === "tokens" || x.unit === "usd") && typeof x.amount === "number" && Number.isFinite(x.amount) && x.amount > 0 && (x.action === "warn" || x.action === "ask" || x.action === "switch");
}

/** Read whatever is in state.json defensively (older files have no budgets). */
export function normalizeBudgets(v: unknown): Budgets {
  const x = (v ?? {}) as Partial<Budgets>;
  const projects: Record<string, Budget> = {};
  for (const [k, b] of Object.entries(x.projects ?? {})) if (validBudget(b)) projects[k] = b;
  return { overall: validBudget(x.overall) ? x.overall : null, projects };
}

/** "1M", "250k", "1.5M", "900" — budget amounts in tokens read better than formatTokens' "1000k". */
export function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 2)}M`;
  if (n >= 1000) return `${+(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(Math.round(n));
}

export function formatAmount(unit: BudgetUnit, n: number): string {
  return unit === "usd" ? `$${n < 100 ? n.toFixed(2) : Math.round(n).toLocaleString("en")}` : compactTokens(n);
}

/** "1M", "250k", "1,000,000", "$20" → a positive number, else null. */
export function parseAmount(text: string): number | null {
  const m = text.trim().replace(/[$,_\s]/g, "").match(/^(\d+(?:\.\d+)?)([kKmM]?)$/);
  if (!m) return null;
  const n = Number(m[1]) * (m[2] ? (m[2].toLowerCase() === "k" ? 1e3 : 1e6) : 1);
  return n > 0 && Number.isFinite(n) ? n : null;
}

/** A budget that applies to a chat, with what it has used. */
export interface BudgetCheck {
  scope: "project" | "overall";
  /** The project's name, for messages; "" for overall. */
  name: string;
  budget: Budget;
  used: number;
  fraction: number;
  state: BudgetState;
}

/** The budgets that apply to a chat in `projectId` (or none), most used first. */
export function checksFor(budgets: Budgets, spend: { byProject: Record<string, Spend>; total: Spend }, projectId: string | null | undefined, projectName = ""): BudgetCheck[] {
  const out: BudgetCheck[] = [];
  const add = (scope: BudgetCheck["scope"], name: string, budget: Budget, s: Spend) => {
    const fraction = percentUsed(budget, s);
    out.push({ scope, name, budget, used: usedOf(budget, s), fraction, state: budgetState(fraction) });
  };
  const pb = projectId ? budgets.projects[projectId] : undefined;
  if (projectId && pb) add("project", projectName, pb, spend.byProject[projectId] ?? ZERO_SPEND);
  if (budgets.overall) add("overall", "", budgets.overall, spend.total);
  return out.sort((a, b) => b.fraction - a.fraction);
}

export type SendGate = { kind: "send" } | { kind: "ask"; checks: BudgetCheck[] } | { kind: "switch"; model: string; check: BudgetCheck };

/**
 * What to do with a send, given the checks that apply. Only budgets at ≥100% act: "ask" wins
 * (the strictest), then "switch" to a cheaper model the chat isn't already on, else just send.
 */
export function sendGate(checks: BudgetCheck[], currentModel: string | null | undefined, usable?: (model: string) => boolean): SendGate {
  const over = checks.filter((c) => c.state === "over");
  const ask = over.filter((c) => c.budget.action === "ask");
  if (ask.length) return { kind: "ask", checks: ask };
  for (const c of over) {
    const m = c.budget.action === "switch" ? c.budget.fallbackModel : null;
    if (m && m !== currentModel && (!usable || usable(m))) return { kind: "switch", model: m, check: c };
  }
  return { kind: "send" };
}

/** Month-to-date spend, loaded on demand and after turns (debounced; one request at a time). */
export class BudgetSpend extends Emitter {
  byProject: Record<string, Spend> = {};
  total: Spend = ZERO_SPEND;
  loadedAt = 0;
  month = "";
  error: string | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private inflight: Promise<void> | null = null;
  private again = false;

  constructor(private query: (p: UsageQueryParams) => Promise<UsageQueryResult>) {
    super();
  }

  /** Refresh after `delayMs` (coalescing calls), or skip when loaded within `maxAgeMs`. */
  refresh(delayMs = 0, maxAgeMs = 0) {
    if (maxAgeMs > 0 && this.month === monthKey() && Date.now() - this.loadedAt < maxAgeMs) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.load();
    }, delayMs);
  }

  load(): Promise<void> {
    if (this.inflight) {
      this.again = true;
      return this.inflight;
    }
    const now = new Date();
    this.inflight = this.query(monthQuery(now))
      .then((r) => {
        const s = spendFromRows(r.rows);
        this.byProject = s.byProject;
        this.total = s.total;
        this.month = monthKey(now);
        this.loadedAt = Date.now();
        this.error = null;
      })
      .catch((e: unknown) => {
        this.error = e instanceof Error ? e.message : String(e);
      })
      .finally(() => {
        this.inflight = null;
        this.changed();
        if (this.again) {
          this.again = false;
          void this.load();
        }
      });
    return this.inflight;
  }
}
