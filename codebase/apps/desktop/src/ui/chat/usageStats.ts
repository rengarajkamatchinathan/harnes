// Token / time / cost maths for a reply and for a whole conversation (pure, for tests).
import type { MessageView, SessionModel, TurnUsage } from "../../state/sessionModel";

export const fmtTokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : String(n));

/** "3.4s", "1m 12s", "1h 5m". */
export function fmtDuration(ms: number): string {
  if (ms < 1000) return `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 100) / 10;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${Math.round(s - m * 60)}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

const ZERO: TurnUsage = { input: 0, output: 0, reasoning: 0, cacheRead: 0, usd: 0, steps: 0 };

/** Another assistant message continues this one's turn (a later tool step), so this isn't its end. */
function continued(msg: MessageView, model: SessionModel): boolean {
  return model.messages.some((m) => m.role === "assistant" && m !== msg && (m.parentId === msg.id || (!!msg.turnId && m.turnId === msg.turnId && m.createdAt > msg.createdAt)));
}

/** The whole turn's tokens (from its `step.ended` events, else the message's own) plus how long it took. */
export function turnStats(msg: MessageView, model: SessionModel): TurnUsage | undefined {
  // One stats line per turn: under its final assistant message only.
  if (continued(msg, model)) return undefined;
  const turn = msg.turnId ? model.turnUsage.get(msg.turnId) : undefined;
  const hasTurnTokens = !!turn && turn.steps > 0;
  const base = hasTurnTokens ? turn : (msg.usage ?? turn ?? ZERO);
  return { ...base, ms: turn?.ms ?? base.ms ?? historyMs(msg, model) };
}

/**
 * Replies loaded from history carry no turn events: estimate the time from the user's message
 * to this reply (walking back through tool steps to the message that started the turn).
 */
export function historyMs(msg: MessageView, model: SessionModel): number | undefined {
  const byId = new Map(model.messages.map((m) => [m.id, m]));
  let p = msg.parentId ? byId.get(msg.parentId) : undefined;
  for (let i = 0; p && p.role !== "user" && i < 50; i++) p = p.parentId ? byId.get(p.parentId) : undefined;
  if (!p) return undefined;
  const ms = Date.parse(msg.createdAt) - Date.parse(p.createdAt);
  return Number.isFinite(ms) && ms >= 0 && ms < 6 * 3600_000 ? ms : undefined;
}

export interface ConversationUsage extends TurnUsage {
  /** Replies (turns) counted. */
  replies: number;
  /** Total time spent answering, and the longest reply. */
  ms: number;
  longestMs: number;
}

/** Everything this chat or session used so far: tokens of every model call, time of every reply. */
export function conversationUsage(model: SessionModel): ConversationUsage {
  const out: ConversationUsage = { ...ZERO, replies: 0, ms: 0, longestMs: 0 };
  const add = (u: TurnUsage) => {
    out.input += u.input;
    out.output += u.output;
    out.reasoning += u.reasoning;
    out.cacheRead += u.cacheRead;
    out.usd += u.usd;
    out.steps += u.steps || 1;
  };
  // Tokens: per turn, the live `step.ended` totals when we have them, else every saved message's own usage.
  const counted = new Set<string>();
  for (const m of model.messages) {
    if (m.role !== "assistant") continue;
    const live = m.turnId ? model.turnUsage.get(m.turnId) : undefined;
    if (live && live.steps > 0) {
      if (!counted.has(m.turnId!)) add(live);
      counted.add(m.turnId!);
    } else if (m.usage) add(m.usage);
  }
  // Live turns whose messages aren't loaded (still streaming) count too.
  for (const [id, u] of model.turnUsage) if (u.steps > 0 && !counted.has(id) && !model.messages.some((m) => m.turnId === id)) add(u);
  // Time and reply count: once per turn, at its final message.
  for (const m of model.messages) {
    if (m.role !== "assistant" || continued(m, model)) continue;
    out.replies += 1;
    const ms = turnStats(m, model)?.ms;
    if (ms != null) {
      out.ms += ms;
      out.longestMs = Math.max(out.longestMs, ms);
    }
  }
  return out;
}
