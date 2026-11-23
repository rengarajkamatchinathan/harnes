// The context meter's "what's using it" breakdown (inside the usage popover) and the banner
// above the composer once a conversation fills ≥ 70% of the model's context, with one-click
// ways to shrink it: compact (engine `session.compact`, optional focus) or continue in a new chat.
import { useMemo, useState } from "react";
import { Banner, Button, Icon, IconButton, Input } from "@genie/ui";
import { t } from "../../i18n";
import type { SessionStore } from "../../state/session";
import type { App } from "../../state/app";
import { useApp } from "../context";
import { analyzeContext, contextLevel, handoffPrompt, TRIM_CATEGORIES, type ContextAnalysis, type ContextLevel, type TrimCategory } from "./contextTrim";
import { scrollToMessage } from "./scrollBus";
import { fmtTokens } from "./usageStats";

const CAT_KEY: Record<TrimCategory, Parameters<typeof t>[0]> = {
  user: "ctx.cat.user",
  reply: "ctx.cat.reply",
  tool: "ctx.cat.tool",
  media: "ctx.cat.media",
  reasoning: "ctx.cat.reasoning",
  system: "ctx.cat.system",
};

export function useContextAnalysis(store: SessionStore): ContextAnalysis {
  const m = store.model;
  return useMemo(() => analyzeContext(m), [m, m.rev, m.messages, m.usage]); // eslint-disable-line react-hooks/exhaustive-deps
}

/** Stacked bar by category + the 5 heaviest items (click → scroll the transcript there). */
export function ContextBreakdown({ store, onJump }: { store: SessionStore; onJump?: () => void }) {
  const a = useContextAnalysis(store);
  if (!a.total || !a.items.length) return null;
  const cats = TRIM_CATEGORIES.filter((c) => a.byCategory[c] > 0);
  const top = a.items.slice(0, 5);
  const huge = top.some((i) => i.huge);
  return (
    <div className="ctxuse">
      <span className="usagepop__title">{t("ctx.whatsUsing")}</span>
      <div className="ctxuse__bar" role="img" aria-label={cats.map((c) => `${t(CAT_KEY[c])} ${Math.round((a.byCategory[c] / a.total) * 100)}%`).join(", ")}>
        {cats.map((c) => (
          <span key={c} className={`ctxuse__seg ctxuse__seg--${c}`} style={{ flexGrow: a.byCategory[c] }} />
        ))}
      </div>
      <ul className="ctxuse__legend">
        {cats.map((c) => (
          <li key={c}>
            <span className={`ctxuse__dot ctxuse__seg--${c}`} />
            {t(CAT_KEY[c])} <span className="muted">{fmtTokens(a.byCategory[c])}</span>
          </li>
        ))}
      </ul>
      <ol className="ctxuse__items">
        {top.map((i) => (
          <li key={`${i.messageId}:${i.label}:${i.tokens}`}>
            <button
              type="button"
              className="ctxuse__item"
              title={t("ctx.jump")}
              onClick={() => {
                onJump?.();
                scrollToMessage(store.id, i.messageId);
              }}
            >
              <span className={`ctxuse__dot ctxuse__seg--${i.category}`} />
              <span className="ctxuse__label">{i.label}</span>
              <span className="ctxuse__num">
                {fmtTokens(i.tokens)} · {Math.max(1, Math.round(i.share * 100))}%
              </span>
            </button>
          </li>
        ))}
      </ol>
      {huge && (
        <p className="ctxuse__hint">
          <Icon name="info" size={12} /> {t("ctx.hugeHint")}
        </p>
      )}
      {!a.scaled && <p className="ctxuse__hint muted">{t("ctx.estimate")}</p>}
    </div>
  );
}

/** Dismissed banners, per session (in memory): dismissing the gentle one still lets the strong one show. */
const dismissed = new Map<string, ContextLevel>();

export async function compactSession(app: App, store: SessionStore, focus?: string) {
  try {
    await app.client.session.compact({ session_id: store.id, focus: focus?.trim() || null });
    app.toast({ title: t("slash.compacting"), tone: "info" });
  } catch (e) {
    app.toast({ title: app.describeError(e), tone: "danger" });
  }
}

/** New session with the same kind / project / workspace / model, seeded with the latest messages. */
export async function continueInNewChat(app: App, store: SessionStore) {
  const info = store.model.info;
  if (!info) return;
  const prompt = handoffPrompt(store.model.messages, 12000, info.title);
  try {
    app.toast({ title: t("ctx.continuing"), tone: "info" });
    const id =
      info.kind === "code"
        ? await app.createSession({ kind: "code", workspacePath: info.workspace_id || undefined })
        : await app.createSession({ kind: "chat", projectId: info.project_id ?? null });
    if (info.model) await app.client.session.update({ session_id: id, model: info.model }).catch(() => undefined);
    const next = app.session(id);
    if (next.loading)
      await new Promise<void>((resolve) => {
        const off = next.subscribe(() => {
          if (!next.loading) {
            off();
            resolve();
          }
        });
      });
    await next.send([{ type: "text", text: prompt }]);
  } catch (e) {
    app.toast({ title: app.describeError(e), tone: "danger" });
  }
}

/** Gentle at ≥ 70% of the window, stronger at ≥ 90%; hidden while a turn is running. */
export function ContextBanner({ store, readOnly }: { store: SessionStore; readOnly?: boolean }) {
  const app = useApp();
  const ctx = store.model.usage;
  const ratio = ctx?.context_window ? ctx.context_tokens / ctx.context_window : null;
  const level = contextLevel(ratio);
  const [, setTick] = useState(0);
  const [focusOpen, setFocusOpen] = useState(false);
  const [focus, setFocus] = useState("");
  if (level === "ok" || readOnly || store.model.status === "compacting") return null;
  const gone = dismissed.get(store.id);
  if (gone === level || (gone === "critical" && level === "warn")) return null;
  const pct = Math.round((ratio ?? 0) * 100);
  const code = store.model.info?.kind === "code";
  const busy = store.busy;
  const compact = () => {
    setFocusOpen(false);
    void compactSession(app, store, focus).then(() => setFocus(""));
  };
  return (
    <div className={`ctxbanner ctxbanner--${level}`}>
      <Banner
        tone={level === "critical" ? "danger" : "warning"}
        icon="warning"
        role="status"
        actions={
          <>
            <Button size="sm" variant={level === "critical" ? "primary" : "secondary"} disabled={busy} onClick={() => (focusOpen ? compact() : setFocusOpen(true))}>
              {focusOpen ? t("ctx.compactGo") : t("ctx.compact")}
            </Button>
            <Button size="sm" disabled={busy} onClick={() => void continueInNewChat(app, store)}>
              {code ? t("ctx.continueNewCode") : t("ctx.continueNew")}
            </Button>
            <IconButton
              icon="x"
              size="sm"
              label={t("ctx.dismiss")}
              onClick={() => {
                dismissed.set(store.id, level);
                setTick((n) => n + 1);
              }}
            />
          </>
        }
      >
        {t(level === "critical" ? "ctx.critical" : "ctx.warn", { pct })}
      </Banner>
      {focusOpen && (
        <form
          className="ctxbanner__focus"
          onSubmit={(e) => {
            e.preventDefault();
            compact();
          }}
        >
          <Input autoFocus value={focus} placeholder={t("ctx.compactFocus")} aria-label={t("ctx.compactFocus")} onChange={(e) => setFocus(e.target.value)} onKeyDown={(e) => e.key === "Escape" && setFocusOpen(false)} />
        </form>
      )}
    </div>
  );
}
