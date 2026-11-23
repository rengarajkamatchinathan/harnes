// "Ready to commit" checklist (Code sessions): pure helpers that classify the last turn's tool
// calls (did it change files? run tests? lint?), parse diff stats and draft a commit message.
import type { ToolOutcome } from "@genie/protocol";
import type { SessionModel } from "../../state/sessionModel";
import { resultText } from "../chat/toolInfo";

export type CheckState = "ok" | "failed" | "not_run";
export type CommandKind = "test" | "lint" | null;

export interface TurnCall {
  name: string;
  input: unknown;
  outcome: ToolOutcome | null;
  /** The tool result as text ("" when unknown). */
  result: string;
}

const EDIT_TOOLS = ["edit", "write", "apply_patch", "multi_edit", "patch", "str_replace", "create_file", "notebook_edit"];
const SHELL_TOOLS = ["shell", "bash", "powershell"];

const str = (v: unknown) => (typeof v === "string" ? v : "");
const inputOf = (input: unknown) => (input && typeof input === "object" ? (input as Record<string, unknown>) : {});

export function commandOf(call: Pick<TurnCall, "name" | "input">): string | null {
  if (!SHELL_TOOLS.includes(call.name.toLowerCase())) return null;
  const inp = inputOf(call.input);
  return str(inp.command) || str(inp.cmd) || null;
}

const TEST_RE =
  /(^|[\s;&|(])((npm|pnpm|yarn|bun)\s+(run\s+)?test\b|npx\s+(vitest|jest)\b|vitest\b|jest\b|mocha\b|pytest\b|python3?\s+-m\s+(pytest|unittest)\b|cargo\s+(test|nextest)\b|go\s+test\b|dotnet\s+test\b|mvn\w*\s+(\S+\s+)*test\b|(gradle|gradlew|\.\/gradlew)\s+(\S+\s+)*test\b|rspec\b|phpunit\b|deno\s+test\b|swift\s+test\b|mix\s+test\b|ctest\b|tox\b)/i;
const LINT_RE =
  /(^|[\s;&|(])((npm|pnpm|yarn|bun)\s+(run\s+)?(lint|typecheck|type-check|check-types|tsc)\b|eslint\b|tsc\b|biome\s+(check|lint)\b|cargo\s+(clippy|check|fmt\s+--check)\b|ruff\b|flake8\b|pylint\b|mypy\b|pyright\b|golangci-lint\b|go\s+vet\b|stylelint\b|dotnet\s+format\b|rubocop\b)/i;

/** Is this shell command a test run or a lint/typecheck run? Tests win when a command does both. */
export function classifyCommand(cmd: string): CommandKind {
  if (TEST_RE.test(cmd)) return "test";
  if (LINT_RE.test(cmd)) return "lint";
  return null;
}

const MODIFY_RE = /(^|[\s;&|])(sed\s+(-\S*\s+)*-i|perl\s+-\S*i|mv|cp|rm|touch|mkdir|tee|truncate|patch|git\s+(apply|checkout|restore|mv|rm)|npm\s+(i|install|uninstall)|pnpm\s+(add|remove|i|install)|yarn\s+(add|remove)|bun\s+(add|remove|install|i)|cargo\s+(add|fmt)\b|prettier\s+(\S+\s+)*--write|eslint\s+(\S+\s+)*--fix|ruff\s+format|black|gofmt\s+-w|Set-Content|Out-File|New-Item|Remove-Item|Move-Item|Copy-Item)\b|[^<>2&]>>?\s*[^&\s|]/;

/** Did this call (probably) change files in the working tree? */
export function changesFiles(call: Pick<TurnCall, "name" | "input" | "outcome">): boolean {
  if (call.outcome && call.outcome !== "ok") return false;
  if (EDIT_TOOLS.includes(call.name.toLowerCase())) return true;
  const cmd = commandOf(call);
  return !!cmd && MODIFY_RE.test(cmd);
}

const FAIL_RE = /(\b[1-9]\d*\s+(failed|failing|failures?|errors?)\b|\bFAIL(ED)?\b|\btest result: FAILED|\bTests?:\s+\d+\s+failed|\berror(\[\w+\])?:|\berror\s+TS\d+|\bnpm ERR!|exit(ed)?\s+(with\s+)?(code|status)\s*[1-9]|\bTraceback \(most recent call last\))/;

/** Did the run succeed: outcome ok and no obvious failure markers in its output. */
export function runSucceeded(call: Pick<TurnCall, "outcome" | "result">): boolean {
  if (call.outcome !== "ok") return false;
  return !FAIL_RE.test(call.result);
}

export interface TurnChecks {
  changedFiles: boolean;
  tests: CheckState;
  lint: CheckState;
  /** The last test / lint command that ran, for the tooltip. */
  testCommand?: string;
  lintCommand?: string;
}

/** The checklist state from one turn's tool calls; the last run of each kind decides. */
export function turnChecks(calls: TurnCall[]): TurnChecks {
  const out: TurnChecks = { changedFiles: false, tests: "not_run", lint: "not_run" };
  for (const c of calls) {
    if (changesFiles(c)) out.changedFiles = true;
    const cmd = commandOf(c);
    if (!cmd) continue;
    // A declined / blocked command never ran.
    if (c.outcome === "declined" || c.outcome === "blocked" || c.outcome === null) continue;
    const kind = classifyCommand(cmd);
    if (kind === "test") {
      out.tests = runSucceeded(c) ? "ok" : "failed";
      out.testCommand = cmd;
    } else if (kind === "lint") {
      out.lint = runSucceeded(c) ? "ok" : "failed";
      out.lintCommand = cmd;
    }
  }
  return out;
}

export interface FileStat {
  path: string;
  add: number;
  del: number;
  binary?: boolean;
}

export interface DiffStats {
  files: FileStat[];
  add: number;
  del: number;
}

/** Per-file +/− line counts from a unified diff (git's, or plain `---`/`+++` headers). */
export function diffStats(diff: string): DiffStats {
  const files: FileStat[] = [];
  const git = /^diff --git /m.test(diff);
  const lines = diff.split(/\r?\n/);
  let cur: FileStat | null = null;
  let inHunk = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const head = git ? /^diff --git a\/(.+?) b\/(.+)$/.exec(line) : null;
    // Plain diffs: a `---` line directly followed by `+++` starts a file.
    const plain = !git && line.startsWith("--- ") && !!lines[i + 1]?.startsWith("+++ ");
    if (head || plain) {
      const path = head ? head[2] : lines[i + 1].slice(4).replace(/^b\//, "").replace(/\t.*$/, "");
      cur = { path: path.trim(), add: 0, del: 0 };
      files.push(cur);
      inHunk = false;
      if (plain) i++;
      continue;
    }
    if (!cur) continue;
    if (line.startsWith("@@")) {
      inHunk = true;
      continue;
    }
    if (!inHunk) {
      if (line.startsWith("Binary files")) cur.binary = true;
      continue;
    }
    if (line.startsWith("+")) cur.add++;
    else if (line.startsWith("-")) cur.del++;
  }
  return { files, add: files.reduce((n, f) => n + f.add, 0), del: files.reduce((n, f) => n + f.del, 0) };
}

const TYPE_HINTS: [RegExp, string][] = [
  [/\b(fix(es|ed)?|bug|broken|crash|error|issue|regression)\b/i, "fix"],
  [/\b(tests?|spec|coverage)\b/i, "test"],
  [/\b(docs?|readme|documentation|comment)\b/i, "docs"],
  [/\b(refactor|clean ?up|rename|simplif(y|ied)|restructure|extract)\b/i, "refactor"],
  [/\b(perf|performance|faster|speed up|optimi[sz]e)\b/i, "perf"],
  [/\b(style|format(ting)?|lint)\b/i, "style"],
  [/\b(build|deps?|dependenc(y|ies)|bump|upgrade|ci)\b/i, "chore"],
];

const isDoc = (p: string) => /\.(md|mdx|rst|txt)$/i.test(p) || /(^|\/)docs?\//i.test(p);
const isTest = (p: string) => /(\.|_)(test|spec)\.\w+$|(^|\/)(tests?|__tests__|spec)\//i.test(p);

/** Conventional-commit type from the prompt / title, else from which files changed. */
export function commitType(text: string, paths: string[]): string {
  for (const [re, type] of TYPE_HINTS) if (re.test(text)) return type;
  if (paths.length && paths.every(isDoc)) return "docs";
  if (paths.length && paths.every(isTest)) return "test";
  return "feat";
}

function subjectFrom(text: string): string {
  let s = text
    .split("\n")
    .map((l) => l.trim())
    .find(Boolean) ?? "";
  s = s.replace(/^(please|can you|could you|would you|pls)\s+/i, "").replace(/[.!?:;,\s]+$/, "");
  s = s.replace(/^(fix|feat|docs|refactor|perf|style|chore|test)(\([^)]*\))?:\s*/i, "");
  if (s) s = s[0].toLowerCase() + s.slice(1);
  return s.length > 64 ? `${s.slice(0, 63).replace(/\s+\S*$/, "")}…` : s;
}

/** A conventional-commit message: `type: subject` + the changed files as the body. */
export function commitMessage(o: { title?: string | null; prompt?: string | null; files: string[] }): string {
  const source = (o.title ?? "").trim() || (o.prompt ?? "").trim();
  const type = commitType(`${o.title ?? ""}\n${o.prompt ?? ""}`, o.files);
  const subject = subjectFrom(source) || (o.files.length === 1 ? `update ${o.files[0].split("/").pop()}` : `update ${o.files.length} files`);
  const shown = o.files.slice(0, 10);
  const body = shown.length ? `\n\n${shown.map((f) => `- ${f}`).join("\n")}${o.files.length > shown.length ? `\n- …and ${o.files.length - shown.length} more` : ""}` : "";
  return `${type}: ${subject}${body}`;
}

/** The last turn's user prompt and tool calls (input, outcome, result text), newest turn only. */
export function lastTurn(model: Pick<SessionModel, "messages" | "tools">): { prompt: string; calls: TurnCall[] } {
  const msgs = model.messages;
  let start = msgs.length - 1;
  while (start >= 0 && !(msgs[start].role === "user" && !msgs[start].synthetic)) start--;
  const turn = msgs.slice(start + 1);
  const prompt = start >= 0 ? msgs[start].parts.filter((p) => p.kind === "text").map((p) => p.text).join("\n") : "";
  const results = new Map<string, unknown>();
  for (const m of turn) for (const p of m.parts) if (p.final?.type === "tool_result") results.set(p.id, p.final.content);
  const calls: TurnCall[] = [];
  for (const m of turn)
    for (const p of m.parts) {
      if (p.final?.type !== "tool_call") continue;
      const tool = model.tools.get(p.final.call_id);
      const content = tool?.resultPartId ? results.get(tool.resultPartId) : undefined;
      calls.push({ name: p.final.name, input: p.final.input, outcome: tool?.outcome ?? null, result: resultText(content) });
    }
  return { prompt, calls };
}
