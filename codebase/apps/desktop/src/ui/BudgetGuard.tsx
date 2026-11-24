// Budget guard UI: the editor (project page card, Settings → Usage), progress, and the
// warning shown above the composer. Maths and storage rules live in state/budget.ts.
import { useEffect, useState } from "react";
import { Button, Field, Icon, Input, Progress, Segmented, Select, SettingsGroup } from "@genie/ui";
import { t } from "../i18n";
import type { App } from "../state/app";
import { useEmitter } from "../state/emitter";
import { checksFor, daysLeftInMonth, formatAmount, parseAmount, percentUsed, budgetState, usedOf, ZERO_SPEND, type Budget, type BudgetAction, type BudgetCheck, type BudgetUnit, type Budgets } from "../state/budget";
import { useApp } from "./context";

/** Month-to-date spend, refreshed when the caller mounts (at most once a minute unless `fresh`). */
function useSpend(app: App, enabled: boolean, fresh = false) {
  useEmitter(app.budgetSpend);
  useEffect(() => {
    if (enabled) app.budgetSpend.refresh(0, fresh ? 0 : 60_000);
  }, [app, enabled, fresh]);
  return app.budgetSpend;
}

/** The budgets that apply to a chat in `projectId` (none for code sessions' folders), once spend has loaded. */
export function useBudgetChecks(projectId: string | null | undefined): BudgetCheck[] {
  const app = useApp();
  const b = app.ui.budgets;
  const any = !!b.overall || (!!projectId && !!b.projects[projectId]);
  const spend = useSpend(app, any);
  if (!any || !spend.loadedAt) return [];
  const name = app.projects.find((p) => p.project_id === projectId)?.name ?? "";
  return checksFor(b, spend, projectId, name);
}

/** "1M-token" / "$50", as used in "its 1M-token monthly budget". */
export const limitLabel = (b: Budget) => (b.unit === "usd" ? formatAmount("usd", b.amount) : t("budget.limitTokens", { n: formatAmount("tokens", b.amount) }));

export function checkMessage(c: BudgetCheck): string {
  const pct = Math.floor(c.fraction * 100);
  const limit = limitLabel(c.budget);
  if (c.scope === "project") return t(c.state === "over" ? "budget.overProject" : "budget.warnProject", { name: c.name || t("budget.title"), pct, limit });
  return t(c.state === "over" ? "budget.overOverall" : "budget.warnOverall", { pct, limit });
}

/** The strongest warning (≥80%) for this chat, shown just above the composer. */
export function BudgetNotice({ checks, projectId }: { checks: BudgetCheck[]; projectId: string | null | undefined }) {
  const app = useApp();
  const c = checks.find((x) => x.state !== "ok");
  if (!c) return null;
  const model = c.budget.fallbackModel ? (app.models.find((m) => m.id === c.budget.fallbackModel)?.name ?? c.budget.fallbackModel) : null;
  const then = c.state !== "over" ? "" : c.budget.action === "ask" ? t("budget.then.ask") : c.budget.action === "switch" && model ? t("budget.then.switch", { model }) : "";
  const open = () => (c.scope === "project" && projectId ? app.setUi({ view: "project", activeProject: projectId }) : app.openDeepLink("genie://settings/usage"));
  return (
    <div className={`budget-notice budget-notice--${c.state}`} role="status">
      <Icon name="warning" size={14} />
      <span className="grow">
        {checkMessage(c)} {then}
      </span>
      <button className="textbtn budget-notice__link" onClick={open}>
        {t("budget.view")}
      </button>
    </div>
  );
}

/** Bar with "used of limit · N% used" and days left in the month. */
export function BudgetMeter({ budget, spent }: { budget: Budget; spent: { tokens: number; usd: number } }) {
  const f = percentUsed(budget, spent);
  const state = budgetState(f);
  const days = daysLeftInMonth();
  return (
    <div className={`budget-meter budget-meter--${state}`}>
      <Progress value={f} warn={state !== "ok"} label={t("budget.pct", { pct: Math.floor(f * 100) })} />
      <div className="row muted budget-meter__text">
        <span className="grow">{t("budget.used", { used: formatAmount(budget.unit, usedOf(budget, spent)), limit: formatAmount(budget.unit, budget.amount) })}</span>
        <span>{t("budget.pct", { pct: Math.floor(f * 100) })}</span>
      </div>
      <div className="muted budget-meter__text">{days === 1 ? t("budget.dayLeft") : t("budget.daysLeft", { n: days })}</div>
    </div>
  );
}

/** Unit / amount / behaviour at 100% (+ the cheaper model); calls `onSave` with a valid budget. */
export function BudgetForm({ initial, onSave, onCancel, onRemove }: { initial: Budget | null; onSave: (b: Budget) => void; onCancel?: () => void; onRemove?: () => void }) {
  const app = useApp();
  const [unit, setUnit] = useState<BudgetUnit>(initial?.unit ?? "tokens");
  const [amount, setAmount] = useState(initial ? String(initial.amount) : "");
  const [action, setAction] = useState<BudgetAction>(initial?.action ?? "ask");
  const [fallback, setFallback] = useState(initial?.fallbackModel ?? "");
  const [error, setError] = useState<string | null>(null);
  const save = () => {
    const n = parseAmount(amount);
    if (n == null) return setError(t("budget.amountInvalid"));
    if (action === "switch" && !fallback) return setError(t("budget.fallbackNeeded"));
    setError(null);
    onSave({ unit, amount: n, action, fallbackModel: action === "switch" ? fallback : null });
  };
  const models = app.usableModels;
  return (
    <div className="budget-form">
      <Field label={t("budget.unit")}>
        <Segmented label={t("budget.unit")} value={unit} onChange={setUnit} options={(["tokens", "usd"] as const).map((u) => ({ value: u, label: t(`budget.unit.${u}`) }))} />
      </Field>
      <Field label={t("budget.amount")} error={error}>
        <Input value={amount} inputMode="decimal" onChange={(e) => setAmount(e.target.value)} onKeyDown={(e) => e.key === "Enter" && save()} placeholder={unit === "usd" ? t("budget.amountUsd") : t("budget.amountTokens")} aria-label={t("budget.amount")} />
      </Field>
      <Field label={t("budget.action")}>
        <Select aria-label={t("budget.action")} value={action} onChange={(v) => setAction(v as BudgetAction)} options={(["warn", "ask", "switch"] as const).map((a) => ({ value: a, label: t(`budget.action.${a}`) }))} />
      </Field>
      {action === "switch" && (
        <Field label={t("budget.fallback")}>
          <Select aria-label={t("budget.fallback")} value={fallback} placeholder={t("budget.fallbackPick")} onChange={setFallback} options={models.map((m) => ({ value: m.id, label: m.name || m.id, group: m.provider }))} />
        </Field>
      )}
      <div className="row budget-form__actions">
        {onRemove && (
          <Button variant="ghost" onClick={onRemove}>
            {t("budget.remove")}
          </Button>
        )}
        <span className="grow" />
        {onCancel && <Button onClick={onCancel}>{t("common.cancel")}</Button>}
        <Button variant="primary" onClick={save}>
          {t("common.save")}
        </Button>
      </div>
    </div>
  );
}

function setBudgets(app: App, patch: (b: Budgets) => Budgets) {
  app.setUi({ budgets: patch(app.ui.budgets) });
  app.budgetSpend.refresh(0);
}

/** Meter + edit/remove for one budget (`null` = none yet). */
function BudgetPanel({ budget, spent, error, onSave }: { budget: Budget | null; spent: { tokens: number; usd: number } | null; error: string | null; onSave: (b: Budget | null) => void }) {
  const [editing, setEditing] = useState(false);
  if (editing || !budget)
    return editing ? (
      <BudgetForm
        initial={budget}
        onCancel={() => setEditing(false)}
        onRemove={budget ? () => (onSave(null), setEditing(false)) : undefined}
        onSave={(b) => {
          onSave(b);
          setEditing(false);
        }}
      />
    ) : (
      <div className="row">
        <span className="muted grow budget-meter__text">{t("budget.none")}</span>
        <Button size="sm" onClick={() => setEditing(true)}>
          {t("budget.set")}
        </Button>
      </div>
    );
  return (
    <div className="stack-tight">
      {spent ? <BudgetMeter budget={budget} spent={spent} /> : error ? <p className="muted budget-meter__text">{t("budget.unavailable")}</p> : null}
      <div className="row muted budget-meter__text">
        <span className="grow">{t(`budget.action.${budget.action}`)}</span>
        <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>
          {t("budget.edit")}
        </Button>
      </div>
    </div>
  );
}

/** Project page side card. */
export function ProjectBudgetCard({ projectId }: { projectId: string }) {
  const app = useApp();
  const spend = useSpend(app, true, true);
  const budget = app.ui.budgets.projects[projectId] ?? null;
  return (
    <div className="project__card">
      <div className="project__card-head">
        <strong>{t("budget.title")}</strong>
      </div>
      <p className="muted project__meta">{t("budget.projectHint")}</p>
      <BudgetPanel
        budget={budget}
        spent={spend.loadedAt ? (spend.byProject[projectId] ?? ZERO_SPEND) : null}
        error={spend.error}
        onSave={(b) =>
          setBudgets(app, (x) => {
            const projects = { ...x.projects };
            if (b) projects[projectId] = b;
            else delete projects[projectId];
            return { ...x, projects };
          })
        }
      />
    </div>
  );
}

/** Settings → Usage: the overall monthly budget. */
export function OverallBudget() {
  const app = useApp();
  const spend = useSpend(app, true, true);
  return (
    <SettingsGroup title={t("budget.overallTitle")}>
      <div className="budget-overall">
        <p className="muted settings-note">{t("budget.overallHint")}</p>
        <BudgetPanel budget={app.ui.budgets.overall} spent={spend.loadedAt ? spend.total : null} error={spend.error} onSave={(b) => setBudgets(app, (x) => ({ ...x, overall: b }))} />
        <p className="muted settings-note">{t("budget.codeNote")}</p>
      </div>
    </SettingsGroup>
  );
}
