// Turns a reply that asks the user several questions into steps for the question stepper.
// Two sources, in order:
//  1. A fenced ```questions block with JSON (the format the system prompt asks models to use):
//     {"questions":[{"q":"…","options":["…","…"],"multi":false}]}
//  2. Otherwise a numbered list whose items are questions; options come from runs of bold
//     choices like "**Mon–Sun** or **Sun–Sat**" or "**a**, **b**, or a **c**".

export interface Question {
  text: string;
  options: string[];
  /** Several options may be picked. */
  multi?: boolean;
}

const FENCE = /```questions[^\n]*\n([\s\S]*?)```/;

export function parseQuestions(md: string): Question[] | null {
  return fromFence(md) ?? fromList(md);
}

function fromFence(md: string): Question[] | null {
  const m = FENCE.exec(md);
  if (!m) return null;
  try {
    const raw = JSON.parse(m[1]) as unknown;
    const list = Array.isArray(raw) ? raw : (raw as { questions?: unknown }).questions;
    if (!Array.isArray(list)) return null;
    const out: Question[] = [];
    for (const it of list) {
      if (!it || typeof it !== "object") continue;
      const o = it as Record<string, unknown>;
      const text = String(o.q ?? o.question ?? o.text ?? "").trim();
      if (!text) continue;
      const options = Array.isArray(o.options) ? o.options.map((x) => String(x).trim()).filter(Boolean) : [];
      out.push({ text, options, multi: o.multi === true });
    }
    return out.length ? out : null;
  } catch {
    return null;
  }
}

/** Numbered items (with their wrapped/indented continuation lines) of the first list that asks ≥ 2 questions. */
function fromList(md: string): Question[] | null {
  const lines = md.replace(/```[\s\S]*?```/g, "").split("\n");
  const lists: string[][] = [];
  let cur: string[] | null = null;
  for (const line of lines) {
    const item = /^\s{0,3}\d+[.)]\s+(.*)$/.exec(line);
    if (item) {
      if (!cur) lists.push((cur = []));
      cur.push(item[1]);
    } else if (cur && /^\s{2,}\S/.test(line) && !/^\s*[-*+]\s/.test(line)) {
      cur[cur.length - 1] += ` ${line.trim()}`;
    } else if (cur && line.trim() === "") {
      // A blank line may separate items of the same list; a non-item line ends it.
    } else {
      cur = null;
    }
  }
  for (const items of lists) {
    const qs = items.filter((x) => x.includes("?"));
    if (qs.length >= 2) return qs.map((x) => ({ text: plain(x), options: boldChoices(x) }));
  }
  return null;
}

/** "…start **Mon–Sun** or **Sun–Sat**?" → ["Mon–Sun", "Sun–Sat"]: bold items joined only by commas / "or" / "and/or". */
export function boldChoices(s: string): string[] {
  const bold = [...s.matchAll(/\*\*([^*]+)\*\*/g)].map((m) => ({ text: m[1].trim(), start: m.index!, end: m.index! + m[0].length }));
  let best: string[] = [];
  let run: typeof bold = [];
  const flush = () => {
    if (run.length >= 2 && run.length > best.length) best = run.map((b) => b.text);
    run = [];
  };
  for (const b of bold) {
    const prev = run[run.length - 1];
    if (prev && /^\s*,?\s*(?:or|and\/or)?\s*(?:a|an|the)?\s*$/i.test(s.slice(prev.end, b.start)) && /,|\bor\b/i.test(s.slice(prev.end, b.start))) run.push(b);
    else {
      flush();
      run = [b];
    }
  }
  flush();
  return best;
}

/** Markdown emphasis and code marks removed, for display. */
export function plain(s: string): string {
  return s
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/(^|[^*])\*([^*]+)\*/g, "$1$2")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

/** The reply sent back: one numbered line per answered question. */
export function formatAnswers(questions: Question[], answers: string[]): string {
  const lines = questions.map((q, i) => `${i + 1}. ${q.text}\n   → ${answers[i]?.trim() || "(skipped)"}`);
  return `Here are my answers:\n\n${lines.join("\n")}`;
}
