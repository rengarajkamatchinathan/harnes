// Usage views: the per-reply stats line (click → breakdown) and the per-conversation usage
// button by the composer (click → this chat's totals, context window, link to all usage).
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { Icon, Ring } from "@genie/ui";
import { t } from "../../i18n";
import type { TurnUsage } from "../../state/sessionModel";
import type { SessionStore } from "../../state/session";
import { useApp } from "../context";
import { conversationUsage, fmtDuration, fmtTokens } from "./usageStats";
import { ContextBreakdown } from "./ContextMeter";

/** Close a popover on an outside click or Esc. */
function useDismiss(open: boolean, close: () => void, ref: RefObject<HTMLElement | null>) {
  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) close();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("pointerdown", down);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", down);
      document.removeEventListener("keydown", key);
    };
  }, [open, close, ref]);
}

type Row = { k: string; v: string; sub?: boolean; total?: boolean };

/** Token rows (input ↳ cache, output ↳ reasoning, total) plus any extra rows below a rule. */
function Breakdown({ title, u, extra, footer }: { title: string; u: TurnUsage; extra: Row[]; footer?: ReactNode }) {
  const num = (n: number) => n.toLocaleString();
  const rows: Row[] = [
    { k: t("usage.input"), v: num(u.input) },
    ...(u.cacheRead > 0 ? [{ k: t("usage.cached"), v: num(u.cacheRead), sub: true }] : []),
    { k: t("usage.output"), v: num(u.output) },
    ...(u.reasoning > 0 ? [{ k: t("usage.reasoning"), v: num(u.reasoning), sub: true }] : []),
    { k: t("usage.total"), v: num(u.input + u.output), total: true },
  ];
  const list = (rs: Row[], cls = "") => (
    <dl className={`usagepop__list ${cls}`}>
      {rs.map((r) => (
        <div key={r.k} className={`usagepop__row${r.sub ? " usagepop__row--sub" : ""}${r.total ? " usagepop__row--total" : ""}`}>
          <dt>{r.k}</dt>
          <dd>{r.v}</dd>
        </div>
      ))}
    </dl>
  );
  return (
    <>
      <span className="usagepop__title">{title}</span>
      {list(rows)}
      {extra.length > 0 && list(extra, "usagepop__list--extra")}
      {footer}
    </>
  );
}

/** Per-reply tokens, time and cost (05 §2.2, D24), always shown under the reply; click for the breakdown. */
export function UsageChip({ u }: { u: TurnUsage }) {
  const [open, setOpen] = useState(false);
  const boxRef = useRef<HTMLSpanElement>(null);
  useDismiss(open, () => setOpen(false), boxRef);
  const total = u.input + u.output;
  if (total <= 0 && u.ms == null && u.usd <= 0) return null;
  const extra: Row[] = [
    ...(u.ms != null ? [{ k: t("usage.time"), v: fmtDuration(u.ms) }] : []),
    ...(u.steps > 1 ? [{ k: t("usage.steps"), v: String(u.steps) }] : []),
    ...(u.usd > 0 ? [{ k: t("usage.cost"), v: `$${u.usd.toFixed(4)}` }] : []),
  ];
  return (
    <span className="msg__usage" ref={boxRef}>
      <button type="button" className="msg__usage-btn" aria-expanded={open} aria-haspopup="dialog" aria-label={t("usage.details")} onClick={() => setOpen((o) => !o)}>
        {total > 0 && (
          <span className="msg__stat">
            <Icon name="layers" size={12} /> {t("chat.usageTokens", { tokens: fmtTokens(total) })}
            <span className="msg__stat-split">
              {" "}
              ({fmtTokens(u.input)} in · {fmtTokens(u.output)} out)
            </span>
          </span>
        )}
        {u.ms != null && (
          <span className="msg__stat">
            <Icon name="clock" size={12} /> {fmtDuration(u.ms)}
          </span>
        )}
        {u.usd > 0 && <span className="msg__stat">${u.usd >= 0.01 ? u.usd.toFixed(2) : u.usd.toFixed(4)}</span>}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <span className="usagepop" role="dialog" aria-label={t("usage.details")}>
          <Breakdown title={t("usage.details")} u={u} extra={extra} />
        </span>
      )}
    </span>
  );
}

/**
 * This chat's / session's usage by the composer: the context ring plus total tokens; click for
 * the conversation's totals, time spent and context window, and a link to all usage.
 */
export function SessionUsage({ store, code }: { store: SessionStore; code: boolean }) {
  const app = useApp();
  const [open, setOpen] = useState(false);
  const boxRef = useRef<HTMLSpanElement>(null);
  useDismiss(open, () => setOpen(false), boxRef);
  const u = conversationUsage(store.model);
  const ctx = store.model.usage;
  const ratio = ctx?.context_window ? ctx.context_tokens / ctx.context_window : null;
  const total = u.input + u.output;
  if (!total && ratio == null) return null;
  const extra: Row[] = [
    { k: t("usage.replies"), v: String(u.replies) },
    ...(u.steps > u.replies ? [{ k: t("usage.steps"), v: String(u.steps) }] : []),
    ...(u.ms > 0 ? [{ k: t("usage.timeTotal"), v: fmtDuration(u.ms) }] : []),
    ...(u.ms > 0 && u.replies > 0 ? [{ k: t("usage.timeAvg"), v: fmtDuration(u.ms / u.replies) }] : []),
    ...(u.longestMs > 0 && u.replies > 1 ? [{ k: t("usage.timeLongest"), v: fmtDuration(u.longestMs) }] : []),
    ...(ctx?.context_window ? [{ k: t("usage.context"), v: `${fmtTokens(ctx.context_tokens)} / ${fmtTokens(ctx.context_window)} (${Math.round((ratio ?? 0) * 100)}%)` }] : []),
    ...(u.usd > 0 ? [{ k: t("usage.cost"), v: `$${u.usd.toFixed(4)}` }] : []),
  ];
  return (
    <span className="sessusage" ref={boxRef}>
      {/* Only the context ring shows; the token total is in the tooltip and the card. Without a
          known context window the ring stays empty rather than turning into text. */}
      <button
        type="button"
        className="textbtn sessusage__btn sessusage__btn--ring"
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label={code ? t("usage.thisSession") : t("usage.thisChat")}
        title={total > 0 ? t("chat.usageTokens", { tokens: fmtTokens(total) }) : undefined}
        onClick={() => setOpen((o) => !o)}
      >
        <Ring value={ratio ?? 0} size={16} warn={ratio != null && ratio > 0.8} label={ratio != null ? `${Math.round(ratio * 100)}%` : t("chat.usageTokens", { tokens: fmtTokens(total) })} />
      </button>
      {open && (
        <span className="usagepop usagepop--right" role="dialog" aria-label={code ? t("usage.thisSession") : t("usage.thisChat")}>
          <Breakdown
            title={code ? t("usage.thisSession") : t("usage.thisChat")}
            u={u}
            extra={extra}
            footer={
              <>
              <ContextBreakdown store={store} onJump={() => setOpen(false)} />
              <button
                type="button"
                className="usagepop__link"
                onClick={() => {
                  setOpen(false);
                  app.openDeepLink("genie://settings/usage");
                }}
              >
                {t("usage.seeAll")} <Icon name="chevronRight" size={12} />
              </button>
              </>
            }
          />
        </span>
      )}
    </span>
  );
}
