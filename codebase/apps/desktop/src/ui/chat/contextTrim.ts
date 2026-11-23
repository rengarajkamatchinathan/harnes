// What is filling the context window (pure, for tests): a rough per-item token estimate
// (≈ 4 characters per token), grouped by category and ranked, scaled to the engine's own
// `context_tokens` when known. Also the "continue in a new chat" hand-off prompt.
import type { InputPart } from "@genie/protocol";
import type { MessageView, PartView, SessionModel } from "../../state/sessionModel";
import { resultText, summarizeTool } from "./toolInfo";

export type TrimCategory = "user" | "reply" | "tool" | "media" | "reasoning" | "system";
export const TRIM_CATEGORIES: TrimCategory[] = ["user", "reply", "tool", "media", "reasoning", "system"];

export interface TrimItem {
  /** Message to scroll to (the one holding the tool call for tool output). */
  messageId: string;
  category: TrimCategory;
  label: string;
  tokens: number;
  /** Share of the whole context, 0–1. */
  share: number;
  /** A tool output big enough that compaction is the way to shrink it. */
  huge: boolean;
}

export interface ContextAnalysis {
  /** Estimated tokens in context (equals the engine's `context_tokens` when known). */
  total: number;
  window: number | null;
  /** total / window, when the window is known. */
  ratio: number | null;
  byCategory: Record<TrimCategory, number>;
  /** Heaviest first. */
  items: TrimItem[];
  /** True when the estimates were scaled to the engine's count. */
  scaled: boolean;
}

/** ≈ 4 characters per token: good enough to rank items, never shown as exact. */
export const estTokens = (chars: number) => Math.ceil(Math.max(0, chars) / 4);
/** Images cost a roughly fixed amount per provider; other files are unknown, so a middling guess. */
const IMAGE_TOKENS = 1600;
const FILE_TOKENS = 3000;
/** User text this long was almost certainly pasted. */
const PASTE_CHARS = 6000;
/** Tool output at least this big (or ≥ 15% of the context) gets the compaction hint. */
const HUGE_TOKENS = 8000;

const mediaTokens = (mime: string) => (mime.startsWith("image/") ? IMAGE_TOKENS : FILE_TOKENS);

/** Only what is still in context: messages after the latest compaction (its summary included). */
export function liveMessages(messages: MessageView[]): MessageView[] {
  for (let i = messages.length - 1; i >= 0; i--) if (messages[i].parts.some((p) => p.kind === "compaction")) return messages.slice(i);
  return messages;
}

function partChars(p: PartView): number {
  if (p.kind === "tool_result") return p.final?.type === "tool_result" ? resultText(p.final.content).length : p.text.length;
  if (p.kind === "tool_call") return p.final?.type === "tool_call" ? JSON.stringify(p.final.input ?? "").length : p.json.length;
  if (p.kind === "compaction") return p.final?.type === "compaction" ? (p.final.summary ?? "").length : p.text.length;
  return p.text.length;
}

type Ctx = Pick<SessionModel, "messages" | "usage">;

export function analyzeContext(model: Ctx): ContextAnalysis {
  const msgs = liveMessages(model.messages);
  // call_id → the tool call (name, input, message) so a result stored in a later message is labelled and grouped with it.
  const calls = new Map<string, { name: string; input: unknown; messageId: string }>();
  for (const m of msgs)
    for (const p of m.parts)
      if (p.final?.type === "tool_call") calls.set(p.final.call_id, { name: p.final.name, input: p.final.input, messageId: m.id });

  const raw = new Map<string, Omit<TrimItem, "share" | "huge">>();
  const add = (key: string, messageId: string, category: TrimCategory, label: string, tokens: number) => {
    if (tokens <= 0) return;
    const it = raw.get(key);
    if (it) it.tokens += tokens;
    else raw.set(key, { messageId, category, label, tokens });
  };

  for (const m of msgs) {
    const user = m.role === "user";
    let hasMedia = false;
    for (const p of m.parts) {
      const tokens = estTokens(partChars(p));
      switch (p.kind) {
        case "text":
          if (user && p.text.length >= PASTE_CHARS) add(`${m.id}:paste:${p.id}`, m.id, "user", "Pasted text", tokens);
          else add(`${m.id}:text`, m.id, user ? "user" : "reply", user ? (m.synthetic ? "Genie notice" : "Your message") : "Reply", tokens);
          break;
        case "reasoning":
          add(`${m.id}:reasoning`, m.id, "reasoning", "Reasoning", tokens);
          break;
        case "compaction":
          add(`${m.id}:compaction`, m.id, "reply", "Summary of earlier messages", tokens);
          break;
        case "media": {
          hasMedia = true;
          const f = p.final?.type === "media" ? p.final : null;
          const mime = f?.mime ?? "";
          add(`${m.id}:media:${p.id}`, m.id, "media", mime.startsWith("image/") ? `Image${f?.name ? ` ${f.name}` : ""}` : `File${f?.name ? ` ${f.name}` : ""}`, mediaTokens(mime));
          break;
        }
        case "tool_call":
        case "tool_result": {
          const callId = p.final?.type === "tool_call" || p.final?.type === "tool_result" ? p.final.call_id : p.id;
          const call = calls.get(callId);
          add(`tool:${callId}`, call?.messageId ?? m.id, "tool", toolLabel(call?.name ?? "tool", call?.input), tokens);
          break;
        }
        default:
          break;
      }
    }
    if (!hasMedia) for (const ip of m.input ?? []) addInput(ip, m.id, add);
  }

  const rawTotal = [...raw.values()].reduce((n, i) => n + i.tokens, 0);
  const ctxTokens = model.usage?.context_tokens ?? 0;
  const window = model.usage?.context_window || null;
  // Scale the estimates to the engine's count. The engine's count also covers the system prompt
  // and tool definitions, which aren't messages: the factor is clamped and whatever is left over
  // is shown as "Instructions & tools" rather than inflating every message.
  let factor = 1;
  if (ctxTokens > 0 && rawTotal > 0) factor = Math.min(2, Math.max(0.25, ctxTokens / rawTotal));
  const byCategory = Object.fromEntries(TRIM_CATEGORIES.map((c) => [c, 0])) as Record<TrimCategory, number>;
  const scaledItems = [...raw.values()].map((i) => ({ ...i, tokens: Math.round(i.tokens * factor) }));
  for (const i of scaledItems) byCategory[i.category] += i.tokens;
  const sum = scaledItems.reduce((n, i) => n + i.tokens, 0);
  if (ctxTokens > sum) byCategory.system = ctxTokens - sum;
  const total = ctxTokens > 0 ? Math.max(ctxTokens, sum) : sum;
  const items: TrimItem[] = scaledItems
    .map((i) => ({ ...i, share: total ? i.tokens / total : 0, huge: i.category === "tool" && (i.tokens >= HUGE_TOKENS || (total > 0 && i.tokens / total >= 0.15)) }))
    .sort((a, b) => b.tokens - a.tokens);
  return { total, window, ratio: window && ctxTokens > 0 ? ctxTokens / window : null, byCategory, items, scaled: ctxTokens > 0 };
}

function addInput(ip: InputPart, messageId: string, add: (key: string, id: string, c: TrimCategory, label: string, tokens: number) => void) {
  if (ip.type === "attachment") add(`${messageId}:att:${ip.blob_id}`, messageId, "media", ip.mime.startsWith("image/") ? `Image ${ip.name}` : `Pasted file ${ip.name}`, mediaTokens(ip.mime));
}

/** "Tool output: shell `npm test`", "Tool output: read src/app.ts". */
export function toolLabel(name: string, input: unknown): string {
  const inp = (input && typeof input === "object" ? input : {}) as Record<string, unknown>;
  const s = (v: unknown) => (typeof v === "string" ? v : "");
  const clip = (v: string, n = 40) => (v.length > n ? `${v.slice(0, n - 1)}…` : v);
  if (name === "shell" || name === "bash") return `Tool output: shell \`${clip(s(inp.command))}\``;
  const target = s(inp.path) || s(inp.file_path) || s(inp.url) || s(inp.query) || s(inp.pattern);
  if (target) return `Tool output: ${name} ${clip(target)}`;
  const title = summarizeTool(name, input, null).title;
  return `Tool output: ${clip(title || name)}`;
}

export type ContextLevel = "ok" | "warn" | "critical";
/** Gentle banner from 70%, stronger from 90%. */
export function contextLevel(ratio: number | null): ContextLevel {
  if (ratio == null) return "ok";
  return ratio >= 0.9 ? "critical" : ratio >= 0.7 ? "warn" : "ok";
}

/**
 * The first prompt of a "continue in a new chat" hand-off. We don't ask the current session to
 * summarise itself first: that costs a full turn on an already-crowded context, needs us to wait
 * for and capture a streamed reply (and could be compacted or interrupted mid-way). Instead we
 * carry the latest user / assistant text over verbatim, newest first until a character budget is
 * spent, and let the new chat pick up from there. Tool output, reasoning and media are left out —
 * they're what made the old chat heavy.
 */
export function handoffPrompt(messages: MessageView[], budgetChars = 12000, title?: string | null): string {
  const turns = messages
    .filter((m) => !m.synthetic)
    .map((m) => ({
      role: m.role as string,
      text: m.parts
        .filter((p) => p.kind === "text" || p.kind === "compaction")
        .map((p) => (p.kind === "compaction" && p.final?.type === "compaction" ? (p.final.summary ?? "") : p.text))
        .join("\n\n"),
    }));
  return handoffPromptFrom(turns, budgetChars, title);
}

/** The hand-off prompt from plain user/assistant turns (also continues an imported handoff file). */
export function handoffPromptFrom(
  messages: { role: string; text: string }[],
  budgetChars = 12000,
  title?: string | null,
  opts: { intro?: string; extra?: string } = {},
): string {
  const turns: string[] = [];
  let used = 0;
  const perMessage = Math.max(400, Math.floor(budgetChars / 4));
  for (let i = messages.length - 1; i >= 0 && used < budgetChars; i--) {
    const m = messages[i];
    if (m.role !== "user" && m.role !== "assistant") continue;
    let text = m.text.trim();
    if (!text) continue;
    const room = Math.min(perMessage, budgetChars - used);
    if (text.length > room) text = `${text.slice(0, Math.max(0, room - 1))}…`;
    used += text.length;
    turns.unshift(`${m.role === "user" ? "User" : "Genie"}: ${text}`);
  }
  const head =
    opts.intro ??
    `This continues an earlier conversation${title ? ` ("${title}")` : ""} that grew too long. Here are its most recent messages (older ones and tool output are left out):`;
  const extra = opts.extra ? `\n\n${opts.extra}` : "";
  return `${head}\n\n${turns.join("\n\n---\n\n")}${extra}\n\nPlease briefly summarise where we left off, then continue from there.`;
}
