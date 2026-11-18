// Slash menu logic (05 §2.3, 06 §9.1, D58), kept pure for tests: what the user is typing,
// which rows show and in what order, and the recently used list.
import type { CommandInfo } from "@genie/protocol";

/** How many recently used command names are kept (per user, in UI state). */
export const RECENT_MAX = 8;

/** The command being typed: `/` at the very start, then the name, with no whitespace yet. */
export function slashQuery(text: string): string | null {
  const m = /^\/([^\s]*)$/.exec(text);
  return m ? m[1] : null;
}

/** A command at the start of the prompt: `/name` + the rest of the text as arguments. */
export function parseSlash(text: string): { name: string; args: string } | null {
  const m = /^\/([^\s/]+)(?:\s+([\s\S]*))?$/.exec(text.trimEnd());
  return m ? { name: m[1], args: (m[2] ?? "").trim() } : null;
}

/** The command `name` refers to (by name or alias). */
export function findCommand(commands: CommandInfo[], name: string): CommandInfo | undefined {
  return commands.find((c) => c.name === name) ?? commands.find((c) => c.aliases?.includes(name));
}

const SOURCE_ORDER: Record<string, number> = { builtin: 0, project: 1, user: 1, skill: 2, mcp_prompt: 3 };

/** Match quality of a query against a name: prefix 0, word-start 1, substring 2, none -1. */
function matchRank(name: string, q: string): number {
  if (!q) return 0;
  const n = name.toLowerCase();
  if (n.startsWith(q)) return 0;
  if (n.split(/[:\-_]/).some((w) => w.startsWith(q))) return 1;
  return n.includes(q) ? 2 : -1;
}

/**
 * The menu rows (D58). Unfiltered: recently used first, then built-ins, your commands and
 * skills, then connector prompts (engine order within each). Filtered: by match quality
 * on name or alias, then the same order.
 */
export function rankCommands(commands: CommandInfo[], query: string, recent: string[]): CommandInfo[] {
  const q = query.toLowerCase();
  const scored: { c: CommandInfo; match: number; recent: number; source: number; i: number }[] = [];
  commands.forEach((c, i) => {
    const ranks = [c.name, ...(c.aliases ?? [])].map((n) => matchRank(n, q)).filter((r) => r >= 0);
    if (!ranks.length) return;
    const r = recent.indexOf(c.name);
    scored.push({ c, match: Math.min(...ranks), recent: r < 0 ? Infinity : r, source: SOURCE_ORDER[c.source] ?? 4, i });
  });
  scored.sort((a, b) => a.match - b.match || a.recent - b.recent || a.source - b.source || a.i - b.i);
  return scored.map((s) => s.c);
}

/** Move `name` to the front of the recently used list. */
export function pushRecent(recent: string[], name: string): string[] {
  return [name, ...recent.filter((n) => n !== name)].slice(0, RECENT_MAX);
}

/** A required argument is still missing (`<x>` hint, no text yet): the picked command waits. */
export function needsArgs(c: CommandInfo, args: string): boolean {
  return !args && !!c.args_hint?.trim().startsWith("<");
}

/** The grey hint after `/name `: the argument hint until the user types arguments. */
export function hintAfter(c: CommandInfo | undefined, args: string): string | null {
  return c?.args_hint && !args ? c.args_hint : null;
}
