// Model race: one prompt answered by 2–3 models, compared side by side. Desktop-side only:
// send with the current model, then for each extra model switch the session model and
// `session.regenerate` the reply (a sibling branch, D24), then restore the model. Turns run
// one at a time per session, so the models answer in turn. Each answer is captured from the
// live stream when its turn settles (the engine can't return other branches yet).
import type { InputPart, TurnStatus } from "@genie/protocol";
import { Emitter } from "./emitter";
import type { MessageView, SessionModel, TurnUsage } from "./sessionModel";

/** The current model plus up to two more. */
export const MAX_RACE_MODELS = 3;

export type AnswerStatus = "waiting" | "running" | "done" | "failed" | "cancelled" | "skipped";

export interface RaceAnswer {
  model: string;
  status: AnswerStatus;
  turnId: string | null;
  /** The answer's messages (assistant steps), captured when its turn settled. */
  messages: MessageView[];
  usage?: TurnUsage;
  error?: string;
}

export interface Race {
  id: string;
  sessionId: string;
  /** The prompt every model answers. */
  userMessageId: string;
  /** What was sent, to ask again when there is no earlier answer to branch from. */
  parts?: InputPart[];
  answers: RaceAnswer[];
  running: boolean;
  cancelled: boolean;
  /** The answer the conversation continues from, once the user kept one. */
  kept: number | null;
}

/** What a race needs from an open session (a `SessionStore`). */
export interface RaceSession {
  readonly id: string;
  model: SessionModel;
  subscribe: (fn: () => void) => unknown;
  send(parts: InputPart[], delivery?: "steer"): Promise<{ message_id: string }>;
  regenerate(messageId: string): Promise<unknown>;
  interrupt(): Promise<unknown>;
  reload(): Promise<unknown>;
  switchBranch(id: string): Promise<unknown>;
}

export interface RaceDeps {
  /** `session.update {model}`. */
  setModel: (model: string) => Promise<unknown>;
  describeError: (e: unknown) => string;
}

// ------------------------------------------------------------------ pure helpers

/** Models to race: the current one first, then the extras that are usable, distinct, capped. */
export function raceModels(current: string, extras: string[], usable: string[]): string[] {
  const out = current ? [current] : [];
  for (const id of extras) if (usable.includes(id) && !out.includes(id)) out.push(id);
  return out.slice(0, MAX_RACE_MODELS);
}

/** Add or remove an extra model; at most `MAX_RACE_MODELS - 1` extras, never the current one. */
export function toggleExtra(extras: string[], id: string, current: string): string[] {
  if (id === current) return extras;
  if (extras.includes(id)) return extras.filter((x) => x !== id);
  return extras.length >= MAX_RACE_MODELS - 1 ? extras : [...extras, id];
}

/** "2 of 3 answering": the 1-based answer running now (null when none) and how many settled. */
export function raceProgress(race: Pick<Race, "answers">): { running: number | null; settled: number; total: number } {
  const i = race.answers.findIndex((a) => a.status === "running");
  const settled = race.answers.filter((a) => a.status !== "waiting" && a.status !== "running").length;
  return { running: i < 0 ? null : i + 1, settled, total: race.answers.length };
}

/** The assistant messages a turn produced, in order. */
export function turnMessages(model: Pick<SessionModel, "messages">, turnId: string): MessageView[] {
  return model.messages.filter((m) => m.turnId === turnId && m.role !== "user");
}

/** A turn's tokens/time: the live `step.ended` totals, else the saved messages' own usage. */
export function answerUsage(turn: TurnUsage | undefined, messages: MessageView[]): TurnUsage | undefined {
  if (turn && turn.steps > 0) return turn;
  const own = messages.filter((m) => m.usage);
  if (!own.length) return turn;
  const sum: TurnUsage = { input: 0, output: 0, reasoning: 0, cacheRead: 0, usd: 0, steps: 0, ms: turn?.ms };
  for (const m of own) {
    const u = m.usage!;
    sum.input += u.input;
    sum.output += u.output;
    sum.reasoning += u.reasoning;
    sum.cacheRead += u.cacheRead;
    sum.usd += u.usd;
    sum.steps += 1;
  }
  return sum;
}

/** The answer's last message: the branch leaf to switch to when it is kept. */
export function answerLeaf(a: Pick<RaceAnswer, "messages">): string | null {
  return a.messages[a.messages.length - 1]?.id ?? null;
}

/** An assistant reply to regenerate from (any captured answer; the newest first). */
export function branchBase(race: Pick<Race, "answers">): string | null {
  for (let i = race.answers.length - 1; i >= 0; i--) {
    const m = race.answers[i].messages.find((x) => x.role === "assistant" && !x.id.startsWith("live:"));
    if (m) return m.id;
  }
  return null;
}

/** The user message a reply answers (walking up through tool steps). */
export function promptOf(model: Pick<SessionModel, "messages">, msg: MessageView): string | null {
  const byId = new Map(model.messages.map((m) => [m.id, m]));
  let cur: MessageView | undefined = msg;
  for (let i = 0; cur && i < 50; i++) {
    if (cur.role === "user") return cur.id;
    cur = cur.parentId ? byId.get(cur.parentId) : undefined;
  }
  return null;
}

/** How a settled turn reads as a race answer. */
export function answerStatus(status: TurnStatus): AnswerStatus {
  return status === "completed" ? "done" : status === "failed" ? "failed" : "cancelled";
}

// ------------------------------------------------------------------ state

/** The races of this app session, the extras picked for the next send, and the open comparison. */
export class RaceBook extends Emitter {
  /** Extra models for the next send (empty = no race). */
  extras: string[] = [];
  races: Race[] = [];
  /** The race shown in the compare view. */
  shownId: string | null = null;

  setExtras(extras: string[]) {
    this.extras = extras;
    this.changedNow();
  }

  add(race: Race) {
    this.races = [...this.races, race];
    this.changedNow();
  }

  show(id: string | null) {
    this.shownId = id;
    this.changedNow();
  }

  shown(sessionId: string): Race | undefined {
    return this.races.find((r) => r.id === this.shownId && r.sessionId === sessionId);
  }

  running(sessionId: string): boolean {
    return this.races.some((r) => r.sessionId === sessionId && r.running);
  }

  /** The latest race for this prompt with at least two answers. */
  forPrompt(sessionId: string, userMessageId: string | null): Race | undefined {
    if (!userMessageId) return undefined;
    for (let i = this.races.length - 1; i >= 0; i--) {
      const r = this.races[i];
      if (r.sessionId === sessionId && r.userMessageId === userMessageId && r.answers.length > 1) return r;
    }
    return undefined;
  }

  touch() {
    this.changedNow();
  }
}

export const raceBook = new RaceBook();

let raceSeq = 0;

/**
 * Wait for the next turn this client hasn't seen to start (reported via `onStart`) and settle.
 * Resolves as failed if no turn starts within `startTimeoutMs`.
 */
export function waitTurn(s: Pick<RaceSession, "model" | "subscribe">, seen: Set<string>, onStart: (turnId: string) => void, startTimeoutMs = 30_000): Promise<{ turnId: string | null; status: TurnStatus; error: string | null }> {
  return new Promise((resolve) => {
    let turnId: string | null = null;
    let finished = false;
    let unsub: unknown;
    const done = (r: { turnId: string | null; status: TurnStatus; error: string | null }) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      if (typeof unsub === "function") unsub();
      resolve(r);
    };
    const check = () => {
      const m = s.model;
      if (!turnId)
        for (const k of m.turnUsage.keys())
          if (!seen.has(k)) {
            turnId = k;
            seen.add(k);
            onStart(k);
            break;
          }
      const end = turnId ? m.turnEnd.get(turnId) : undefined;
      if (end) done({ turnId, status: end.status, error: end.error?.message ?? null });
    };
    const timer = setTimeout(() => {
      if (!turnId) done({ turnId: null, status: "failed", error: null });
    }, startTimeoutMs);
    unsub = s.subscribe(check);
    check();
  });
}

/**
 * Send `parts` as a race between `models` (the first is the session's current model). Resolves
 * once the prompt is admitted (errors reject, for the composer); the answers then run in the
 * background, tracked in `raceBook`.
 */
export async function startRace(s: RaceSession, parts: InputPart[], models: string[], deps: RaceDeps, book: RaceBook = raceBook): Promise<Race> {
  const seen = new Set(s.model.turnUsage.keys());
  const original = s.model.info?.model ?? models[0];
  if (original !== models[0]) await deps.setModel(models[0]);
  const res = await s.send(parts, "steer");
  const race: Race = {
    id: `race_${++raceSeq}`,
    sessionId: s.id,
    userMessageId: res.message_id,
    parts,
    answers: models.map((model) => ({ model, status: "waiting", turnId: null, messages: [] })),
    running: true,
    cancelled: false,
    kept: null,
  };
  book.add(race);
  book.show(race.id);
  void runAnswers(s, race, seen, original, deps, book);
  return race;
}

/** Answer with each model in turn; whatever happens, restore the session's model at the end. */
export async function runAnswers(s: RaceSession, race: Race, seen: Set<string>, original: string, deps: RaceDeps, book: RaceBook = raceBook): Promise<void> {
  let model = race.answers[0]?.model ?? original;
  try {
    for (let i = 0; i < race.answers.length; i++) {
      const a = race.answers[i];
      if (race.cancelled) {
        a.status = "cancelled";
        continue;
      }
      if (i > 0) {
        // Regenerate needs an earlier answer to branch from. When every model so far failed without
        // replying (e.g. a provider rejected the request), ask the same question again with this model.
        const base = branchBase(race);
        if (!base && !race.parts) {
          a.status = "skipped";
          book.touch();
          continue;
        }
        try {
          await deps.setModel(a.model);
          model = a.model;
          if (base) await s.regenerate(base);
          else race.userMessageId = (await s.send(race.parts!, "steer")).message_id;
        } catch (e) {
          a.status = "failed";
          a.error = deps.describeError(e);
          book.touch();
          continue;
        }
      }
      a.status = "running";
      book.touch();
      const end = await waitTurn(s, seen, (turnId) => {
        a.turnId = turnId;
        book.touch();
        if (race.cancelled) void s.interrupt().catch(() => {});
      });
      a.messages = end.turnId ? turnMessages(s.model, end.turnId) : [];
      a.usage = answerUsage(end.turnId ? s.model.turnUsage.get(end.turnId) : undefined, a.messages);
      a.status = end.turnId ? answerStatus(end.status) : "failed";
      if (end.error) a.error = end.error;
      if (a.status === "cancelled") race.cancelled = true;
      book.touch();
    }
  } finally {
    if (model !== original) await deps.setModel(original).catch(() => {});
    race.running = false;
    book.touch();
    // Show the conversation as the engine has it now (the last answer is the active branch).
    await s.reload().catch(() => {});
    book.touch();
  }
}

/** Stop the race: the running answer is interrupted, the ones still waiting don't start. */
export function cancelRace(s: Pick<RaceSession, "interrupt">, race: Race, book: RaceBook = raceBook) {
  if (!race.running) return;
  race.cancelled = true;
  book.touch();
  void s.interrupt().catch(() => {});
}

/**
 * Continue the conversation from answer `i`: switch the branch unless it is already the
 * active one. Returns false when the engine kept another branch active (no branch switching).
 */
export async function keepAnswer(s: RaceSession, race: Race, i: number, book: RaceBook = raceBook): Promise<boolean> {
  const leaf = answerLeaf(race.answers[i]);
  if (!leaf) return false;
  const last = () => s.model.messages[s.model.messages.length - 1]?.id;
  if (last() !== leaf) await s.switchBranch(leaf);
  const onPath = (id: string | null) => !!id && s.model.messages.some((m) => m.id === id);
  const ok = onPath(leaf);
  // Without branch switching the engine keeps its own active answer: mark that one instead.
  const active = race.answers.findIndex((a) => onPath(answerLeaf(a)));
  race.kept = ok ? i : active >= 0 ? active : null;
  book.touch();
  return ok;
}
