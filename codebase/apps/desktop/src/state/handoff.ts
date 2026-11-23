// Handoff files (pure, for tests): a read-only, redacted JSON snapshot of a session that a
// teammate can open in Genie (replay view) and continue from. Saved locally only.
import type { MessageView, ToolView } from "./sessionModel";
import { resultText, summarizeTool } from "../ui/chat/toolInfo";
import { diffStats } from "../ui/code/commitReady";
import { handoffPromptFrom } from "../ui/chat/contextTrim";

export const HANDOFF_FORMAT = "genie-handoff";
export const HANDOFF_VERSION = 1;
export const TOOL_RESULT_MAX = 4000;
export const DIFF_MAX = 200_000;
export const TRUNCATED = "… (truncated)";

export interface HandoffTool {
  name: string;
  summary: string;
  outcome: string | null;
  result?: string;
}

export interface HandoffUsage {
  input: number;
  output: number;
  usd: number;
}

export interface HandoffMessage {
  role: "user" | "assistant";
  created_at: string;
  text: string;
  reasoning?: string;
  tools: HandoffTool[];
  usage?: HandoffUsage;
}

export interface HandoffStats {
  messages: number;
  tools: number;
  diffFiles?: number;
  added?: number;
  removed?: number;
}

export interface Handoff {
  format: typeof HANDOFF_FORMAT;
  version: number;
  exported_at: string;
  title: string;
  kind: string;
  model: string;
  /** Project / folder display name; `path` only when the exporter chose to include it. */
  place: { name: string | null; path?: string };
  messages: HandoffMessage[];
  diff?: string;
  stats: HandoffStats;
  /** Number of secrets replaced with "[redacted]". */
  redactions: number;
}

export interface HandoffOptions {
  includeTools: boolean;
  includeToolOutput: boolean;
  includeReasoning: boolean;
  includeDiff: boolean;
  fullPath: boolean;
}

export const DEFAULT_HANDOFF_OPTIONS: HandoffOptions = {
  includeTools: true,
  includeToolOutput: false,
  includeReasoning: false,
  includeDiff: true,
  fullPath: false,
};

export interface HandoffInput {
  title: string | null | undefined;
  kind: string;
  model: string | null | undefined;
  place?: { name: string | null; path?: string | null } | null;
  messages: Pick<MessageView, "role" | "createdAt" | "parts" | "usage" | "synthetic">[];
  tools?: Map<string, Pick<ToolView, "outcome">>;
  diff?: string | null;
  now?: Date;
}

// --- redaction -----------------------------------------------------------------------------

const R = "[redacted]";
const RULES: { re: RegExp; to: (m: string, ...g: string[]) => string }[] = [
  { re: /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, to: () => R },
  { re: /sk-[A-Za-z0-9_-]{16,}/g, to: () => R },
  { re: /AKIA[0-9A-Z]{16}/g, to: () => R },
  { re: /gh[pousr]_[A-Za-z0-9]{20,}/g, to: () => R },
  { re: /xox[abprs]-[A-Za-z0-9-]{10,}/g, to: () => R },
  { re: /Bearer\s+[A-Za-z0-9._~+/=-]{16,}/g, to: () => `Bearer ${R}` },
];
const KEY_VALUE = /((?:api[_-]?key|token|secret|password|passwd)\s*[:=]\s*["']?)([^\s"']{8,})/gi;

/** Replace likely secrets with "[redacted]"; `count` is how many were replaced. */
export function redact(text: string): { text: string; count: number } {
  let count = 0;
  let out = text;
  for (const r of RULES)
    out = out.replace(r.re, (m) => {
      count++;
      return r.to(m);
    });
  out = out.replace(KEY_VALUE, (m, key: string, value: string) => {
    if (value.startsWith(R)) return m;
    count++;
    return `${key}${R}`;
  });
  return { text: out, count };
}

export function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}\n${TRUNCATED}` : text;
}

// --- build ---------------------------------------------------------------------------------

export function buildHandoff(input: HandoffInput, opts: HandoffOptions = DEFAULT_HANDOFF_OPTIONS): Handoff {
  let redactions = 0;
  const clean = (s: string) => {
    const r = redact(s);
    redactions += r.count;
    return r.text;
  };

  // call_id → result (a result may live in a later message than its call).
  const results = new Map<string, { content: unknown; isError: boolean }>();
  for (const m of input.messages)
    for (const p of m.parts)
      if (p.final?.type === "tool_result") results.set(p.final.call_id, { content: p.final.content, isError: !!p.final.is_error });

  let toolCount = 0;
  const messages: HandoffMessage[] = [];
  for (const m of input.messages) {
    if (m.synthetic || (m.role !== "user" && m.role !== "assistant")) continue;
    const text = m.parts
      .filter((p) => p.kind === "text" || p.kind === "compaction")
      .map((p) => (p.kind === "compaction" && p.final?.type === "compaction" ? (p.final.summary ?? "") : p.text))
      .filter(Boolean)
      .join("\n\n")
      .trim();
    const reasoning = m.parts
      .filter((p) => p.kind === "reasoning")
      .map((p) => p.text)
      .filter(Boolean)
      .join("\n\n")
      .trim();
    const tools: HandoffTool[] = [];
    for (const p of m.parts) {
      if (p.final?.type !== "tool_call") continue;
      toolCount++;
      if (!opts.includeTools) continue;
      const { call_id, name, input: args } = p.final;
      const res = results.get(call_id);
      const resText = res ? resultText(res.content) : "";
      const outcome = input.tools?.get(call_id)?.outcome ?? (res ? (res.isError ? "error" : "ok") : null);
      const tool: HandoffTool = { name, summary: clean(summarizeTool(name, args, outcome, resText).title), outcome };
      if (opts.includeToolOutput && res) tool.result = clean(truncate(resText, TOOL_RESULT_MAX));
      tools.push(tool);
    }
    const msg: HandoffMessage = { role: m.role as "user" | "assistant", created_at: m.createdAt, text: clean(text), tools };
    if (opts.includeReasoning && reasoning) msg.reasoning = clean(reasoning);
    if (m.usage) msg.usage = { input: m.usage.input, output: m.usage.output, usd: m.usage.usd };
    if (!msg.text && !msg.tools.length && !msg.reasoning) continue;
    messages.push(msg);
  }

  const stats: HandoffStats = { messages: messages.length, tools: toolCount };
  let diff: string | undefined;
  if (opts.includeDiff && input.diff && input.diff.trim()) {
    const ds = diffStats(input.diff);
    stats.diffFiles = ds.files.length;
    stats.added = ds.add;
    stats.removed = ds.del;
    diff = clean(truncate(input.diff, DIFF_MAX));
  }

  const place: Handoff["place"] = { name: input.place?.name ?? null };
  if (opts.fullPath && input.place?.path) place.path = input.place.path;

  const h: Handoff = {
    format: HANDOFF_FORMAT,
    version: HANDOFF_VERSION,
    exported_at: (input.now ?? new Date()).toISOString(),
    title: clean(input.title?.trim() || "Untitled"),
    kind: input.kind,
    model: input.model ?? "",
    place,
    messages,
    stats,
    redactions: 0,
  };
  if (diff !== undefined) h.diff = diff;
  h.redactions = redactions;
  return h;
}

// --- parse ---------------------------------------------------------------------------------

const str = (v: unknown, d = "") => (typeof v === "string" ? v : d);
const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);

export function parseHandoff(json: string): { ok: true; handoff: Handoff } | { ok: false; error: string } {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return { ok: false, error: "This file isn't valid JSON." };
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return { ok: false, error: "This file isn't a Genie handoff." };
  const o = raw as Record<string, unknown>;
  if (o.format !== HANDOFF_FORMAT) return { ok: false, error: "This file isn't a Genie handoff." };
  const version = typeof o.version === "number" ? o.version : 1;
  if (version > HANDOFF_VERSION) return { ok: false, error: "This handoff was made by a newer version of Genie." };

  const messages: HandoffMessage[] = (Array.isArray(o.messages) ? o.messages : [])
    .filter((m): m is Record<string, unknown> => !!m && typeof m === "object")
    .map((m) => {
      const msg: HandoffMessage = {
        role: m.role === "user" ? "user" : "assistant",
        created_at: str(m.created_at),
        text: str(m.text),
        tools: (Array.isArray(m.tools) ? m.tools : [])
          .filter((t): t is Record<string, unknown> => !!t && typeof t === "object")
          .map((t) => {
            const tool: HandoffTool = { name: str(t.name, "tool"), summary: str(t.summary), outcome: typeof t.outcome === "string" ? t.outcome : null };
            if (typeof t.result === "string") tool.result = t.result;
            return tool;
          }),
      };
      if (typeof m.reasoning === "string" && m.reasoning) msg.reasoning = m.reasoning;
      if (m.usage && typeof m.usage === "object") {
        const u = m.usage as Record<string, unknown>;
        msg.usage = { input: num(u.input), output: num(u.output), usd: num(u.usd) };
      }
      return msg;
    });

  const rawPlace = o.place;
  const place: Handoff["place"] =
    typeof rawPlace === "string"
      ? { name: rawPlace }
      : rawPlace && typeof rawPlace === "object"
        ? { name: str((rawPlace as Record<string, unknown>).name) || null }
        : { name: null };
  if (rawPlace && typeof rawPlace === "object" && typeof (rawPlace as Record<string, unknown>).path === "string")
    place.path = (rawPlace as Record<string, unknown>).path as string;

  const s = (o.stats && typeof o.stats === "object" ? o.stats : {}) as Record<string, unknown>;
  const stats: HandoffStats = {
    messages: typeof s.messages === "number" ? s.messages : messages.length,
    tools: typeof s.tools === "number" ? s.tools : messages.reduce((n, m) => n + m.tools.length, 0),
  };
  if (typeof s.diffFiles === "number") stats.diffFiles = s.diffFiles;
  if (typeof s.added === "number") stats.added = s.added;
  if (typeof s.removed === "number") stats.removed = s.removed;

  const handoff: Handoff = {
    format: HANDOFF_FORMAT,
    version,
    exported_at: str(o.exported_at),
    title: str(o.title) || "Untitled",
    kind: o.kind === "code" ? "code" : "chat",
    model: str(o.model),
    place,
    messages,
    stats,
    redactions: num(o.redactions),
  };
  if (typeof o.diff === "string" && o.diff) {
    handoff.diff = o.diff;
    if (stats.diffFiles === undefined) {
      const ds = diffStats(o.diff);
      stats.diffFiles = ds.files.length;
      stats.added = ds.add;
      stats.removed = ds.del;
    }
  }
  return { ok: true, handoff };
}

// --- continue ------------------------------------------------------------------------------

/** First message for a new session that continues an imported handoff. */
export function handoffContinuePrompt(h: Handoff, budgetChars = 12000): string {
  const intro = `This continues a conversation shared with me as a Genie handoff ("${h.title}"). Here are its most recent messages (older ones and tool output are left out):`;
  let extra: string | undefined;
  if (h.diff) {
    const ds = diffStats(h.diff);
    const files = ds.files.slice(0, 20).map((f) => `- ${f.path} (+${f.add} −${f.del})`);
    const more = ds.files.length > 20 ? `\n- … and ${ds.files.length - 20} more` : "";
    extra = `The original session changed ${ds.files.length} file${ds.files.length === 1 ? "" : "s"} (+${ds.add} −${ds.del}); those changes are not applied here:\n${files.join("\n")}${more}`;
  }
  return handoffPromptFrom(h.messages, budgetChars, h.title, { intro, extra });
}

/** A safe file name: "<title>.genie.json". */
export function handoffFileName(title: string): string {
  const base = title.replace(/[\\/:*?"<>|\u0000-\u001f]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 80) || "handoff";
  return `${base}.genie.json`;
}
