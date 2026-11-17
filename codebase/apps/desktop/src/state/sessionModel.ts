// The view model of one open session, built from `session.get` and kept current by the
// session stream (02 §3–4). Pure: no I/O, no React. Messages are mutated in place and get a
// new `rev` so memoised rows re-render only when they change.
import type {
  Activity,
  ArtifactVersioned,
  Ask,
  BrowserAction,
  BrowserControl,
  BrowserFrame,
  ClientRef,
  ErrorInfo,
  GetResult,
  InboxItemView,
  InputPart,
  LiveStatus,
  Message,
  Part,
  PartKind,
  RetryScheduled,
  Role,
  SessionEventEnvelope,
  SessionInfo,
  ToolOutcome,
  ToolProgress,
  TurnInterrupted,
  UsageUpdated,
} from "@genie/protocol";

export interface PartView {
  id: string;
  kind: PartKind;
  /** Text / reasoning so far (streamed or final). */
  text: string;
  /** Tool input JSON fragments so far. */
  json: string;
  final: Part | null;
  streaming: boolean;
}

export interface MessageView {
  id: string;
  role: Role;
  parentId: string | null;
  turnId: string | null;
  model: string | null;
  createdAt: string;
  parts: PartView[];
  /** What the user sent (attachments, skills) for admitted user messages. */
  input?: InputPart[];
  /** Assistant messages from `session.get`: tokens + cost of the step that produced it. */
  usage?: TurnUsage;
  rev: number;
}

export interface ToolView {
  callId: string;
  name: string;
  resource: string | null;
  outcome: ToolOutcome | null;
  error: ErrorInfo | null;
  progress: ToolProgress | null;
  resultPartId: string | null;
}

export interface SubagentView {
  childSessionId: string;
  agent: string;
  description: string;
  status: "running" | "completed" | "failed" | "cancelled" | "unknown";
  costUsd: number;
  summary: string | null;
}

export interface SessionModel {
  sessionId: string;
  info: SessionInfo | null;
  messages: MessageView[];
  siblings: Record<string, string[]>;
  tools: Map<string, ToolView>;
  status: LiveStatus;
  activity: Activity | null;
  asks: Map<string, Ask>;
  inbox: InboxItemView[];
  usage: UsageUpdated | null;
  retry: (RetryScheduled & { at: number }) | null;
  interrupted: TurnInterrupted | null;
  error: ErrorInfo | null;
  driver: ClientRef | null;
  leaseLost: boolean;
  artifacts: ArtifactVersioned[];
  /** Tokens and cost per turn from `step.ended` events seen by this client (hover card). */
  turnUsage: Map<string, TurnUsage>;
  subagents: Map<string, SubagentView>;
  browser: { control: BrowserControl | null; frame: BrowserFrame | null; actions: BrowserAction[] };
  /** Set by `branch.switched`: the caller reloads with `session.get`. */
  needsReload: boolean;
  synced: boolean;
  rev: number;
}

export interface TurnUsage {
  input: number;
  output: number;
  reasoning: number;
  cacheRead: number;
  usd: number;
  steps: number;
}

export function emptyModel(sessionId: string): SessionModel {
  return {
    sessionId,
    info: null,
    messages: [],
    siblings: {},
    tools: new Map(),
    status: "idle",
    activity: null,
    asks: new Map(),
    inbox: [],
    usage: null,
    retry: null,
    interrupted: null,
    error: null,
    driver: null,
    leaseLost: false,
    artifacts: [],
    turnUsage: new Map(),
    subagents: new Map(),
    browser: { control: null, frame: null, actions: [] },
    needsReload: false,
    synced: false,
    rev: 0,
  };
}

function partText(p: Part): string {
  return p.type === "text" ? p.text : p.type === "reasoning" ? (p.text ?? "") : "";
}

function partView(p: Part): PartView {
  const id = "id" in p ? p.id : "";
  return { id, kind: p.type as PartKind, text: partText(p), json: "", final: p, streaming: false };
}

function messageView(m: Message): MessageView {
  return {
    id: m.id,
    role: m.role,
    parentId: m.parent_id ?? null,
    turnId: m.turn_id ?? null,
    model: m.model ?? null,
    createdAt: m.created_at,
    parts: m.parts.filter((p) => p.type !== "unknown").map(partView),
    ...(m.usage || m.cost
      ? {
          usage: {
            input: m.usage?.input_tokens ?? 0,
            output: m.usage?.output_tokens ?? 0,
            reasoning: m.usage?.reasoning_tokens ?? 0,
            cacheRead: m.usage?.cache_read_tokens ?? 0,
            usd: m.cost?.usd ?? 0,
            steps: 1,
          },
        }
      : {}),
    rev: 0,
  };
}

/** Replace the model's transcript with a `session.get` snapshot. */
export function loadSnapshot(model: SessionModel, snap: GetResult): void {
  model.info = snap.session;
  model.messages = snap.path.map(messageView);
  model.siblings = snap.siblings ?? {};
  model.status = snap.session.status;
  model.driver = snap.session.driver ?? null;
  model.tools = new Map();
  for (const m of model.messages)
    for (const p of m.parts) {
      const f = p.final;
      if (f?.type === "tool_call") model.tools.set(f.call_id, { callId: f.call_id, name: f.name, resource: null, outcome: null, error: null, progress: null, resultPartId: null });
      if (f?.type === "tool_result") {
        const t = model.tools.get(f.call_id);
        if (t) {
          t.outcome = f.is_error ? "error" : "ok";
          t.resultPartId = f.id;
        }
      }
    }
  model.needsReload = false;
  model.synced = false;
  model.rev++;
}

function findMessage(model: SessionModel, id: string): MessageView | undefined {
  for (let i = model.messages.length - 1; i >= 0; i--) if (model.messages[i].id === id) return model.messages[i];
  return undefined;
}

function findPart(model: SessionModel, partId: string): [MessageView, PartView] | undefined {
  for (let i = model.messages.length - 1; i >= 0; i--) {
    const m = model.messages[i];
    for (const p of m.parts) if (p.id === partId) return [m, p];
  }
  return undefined;
}

function upsertMessage(model: SessionModel, id: string, role: Role, env: SessionEventEnvelope, parentId: string | null = null): MessageView {
  let m = findMessage(model, id);
  if (!m) {
    m = { id, role, parentId, turnId: env.turn_id ?? null, model: model.info?.model ?? null, createdAt: env.ts, parts: [], rev: 0 };
    model.messages = [...model.messages, m];
  }
  return m;
}

function touch(m: MessageView) {
  m.rev++;
}

/** Apply one relayed session event. Returns false when it changed nothing visible. */
export function applyEvent(model: SessionModel, env: SessionEventEnvelope): boolean {
  if (env.session_id !== model.sessionId) return false;
  const e = env.event;
  if (env.seq != null && model.info) model.info.head_seq = Math.max(model.info.head_seq, env.seq);
  switch (e.type) {
    case "session.created":
      return false;
    case "session.updated": {
      if (model.info) {
        const { type: _t, ...changes } = e;
        for (const [k, v] of Object.entries(changes)) if (v !== undefined && v !== null) (model.info as unknown as Record<string, unknown>)[k] = v;
      }
      break;
    }
    case "message.admitted": {
      if (findMessage(model, e.message_id)) return false;
      const m = upsertMessage(model, e.message_id, "user", env, e.parent_id ?? null);
      m.input = e.parts;
      m.parts = e.parts
        .filter((p): p is Extract<InputPart, { type: "text" }> => p.type === "text")
        .map((p, i) => ({ id: `${e.message_id}:${i}`, kind: "text", text: p.text, json: "", final: { type: "text", id: `${e.message_id}:${i}`, text: p.text }, streaming: false }));
      touch(m);
      break;
    }
    case "inbox.changed":
      model.inbox = e.items;
      break;
    case "turn.started":
      model.interrupted = null;
      model.error = null;
      model.retry = null;
      break;
    case "step.started":
      return false;
    case "step.ended": {
      if (!env.turn_id) return false;
      const u = model.turnUsage.get(env.turn_id) ?? { input: 0, output: 0, reasoning: 0, cacheRead: 0, usd: 0, steps: 0 };
      const next: TurnUsage = {
        input: u.input + (e.usage.input_tokens ?? 0),
        output: u.output + (e.usage.output_tokens ?? 0),
        reasoning: u.reasoning + (e.usage.reasoning_tokens ?? 0),
        cacheRead: u.cacheRead + (e.usage.cache_read_tokens ?? 0),
        usd: u.usd + (e.cost?.usd ?? 0),
        steps: u.steps + 1,
      };
      model.turnUsage = new Map(model.turnUsage).set(env.turn_id, next);
      break;
    }
    case "part.started": {
      if (findPart(model, e.part_id)) return false;
      const m = upsertMessage(model, e.message_id, "assistant", env);
      m.parts = [...m.parts, { id: e.part_id, kind: e.kind, text: "", json: "", final: null, streaming: true }];
      touch(m);
      break;
    }
    case "part.delta": {
      let found = findPart(model, e.part_id);
      if (!found) {
        // Joined mid-part (its `part.started` is older than our snapshot): show it under
        // the turn's live message.
        const m = upsertMessage(model, `live:${env.turn_id ?? "turn"}`, "assistant", env);
        const p: PartView = { id: e.part_id, kind: e.json != null ? "tool_call" : "text", text: "", json: "", final: null, streaming: true };
        m.parts = [...m.parts, p];
        found = [m, p];
      }
      const [m, p] = found;
      if (p.final) return false;
      if (e.text) p.text += e.text;
      if (e.json) p.json += e.json;
      touch(m);
      break;
    }
    case "part.ended": {
      const found = findPart(model, e.part_id);
      const part = e.part;
      if (!found) {
        const owner = model.messages[model.messages.length - 1];
        if (!owner || owner.role === "user") {
          const m = upsertMessage(model, `live:${env.turn_id ?? "turn"}`, "assistant", env);
          m.parts = [...m.parts, partView(part)];
          touch(m);
        } else {
          owner.parts = [...owner.parts, partView(part)];
          touch(owner);
        }
      } else {
        const [m, p] = found;
        p.final = part;
        p.kind = part.type as PartKind;
        p.text = partText(part) || p.text;
        p.streaming = false;
        touch(m);
      }
      if (part.type === "tool_call" && !model.tools.has(part.call_id))
        model.tools = new Map(model.tools).set(part.call_id, { callId: part.call_id, name: part.name, resource: null, outcome: null, error: null, progress: null, resultPartId: null });
      break;
    }
    case "tool.started": {
      const t = model.tools.get(e.call_id);
      model.tools = new Map(model.tools);
      model.tools.set(e.call_id, { callId: e.call_id, name: e.name, resource: e.resource ?? null, outcome: t?.outcome ?? null, error: null, progress: null, resultPartId: t?.resultPartId ?? null });
      break;
    }
    case "tool.progress": {
      const t = model.tools.get(e.call_id);
      if (!t || t.outcome) return false;
      model.tools = new Map(model.tools).set(e.call_id, { ...t, progress: e });
      break;
    }
    case "tool.ended": {
      const t = model.tools.get(e.call_id) ?? { callId: e.call_id, name: "tool", resource: null, outcome: null, error: null, progress: null, resultPartId: null };
      t.outcome = e.outcome;
      t.error = e.error ?? null;
      t.resultPartId = e.result_part_id;
      t.progress = null;
      model.tools = new Map(model.tools).set(e.call_id, { ...t });
      break;
    }
    case "permission.asked":
      model.asks = new Map(model.asks).set(e.ask.id, e.ask);
      break;
    case "permission.resolved": {
      if (!model.asks.has(e.ask_id)) return false;
      const next = new Map(model.asks);
      next.delete(e.ask_id);
      model.asks = next;
      break;
    }
    case "retry.scheduled":
      model.retry = { ...e, at: Date.now() + e.after_ms };
      break;
    case "turn.settled":
      model.retry = null;
      if (e.status === "failed" && e.error) model.error = e.error;
      for (const m of model.messages)
        for (const p of m.parts)
          if (p.streaming) {
            p.streaming = false;
            touch(m);
          }
      break;
    case "turn.interrupted":
      model.interrupted = e;
      break;
    case "session.compacted":
      break;
    case "artifact.versioned":
      model.artifacts = [...model.artifacts.filter((a) => !(a.artifact_id === e.artifact_id && a.n === e.n)), e];
      break;
    case "memory.changed":
      return false;
    case "subagent.started":
      model.subagents = new Map(model.subagents).set(e.child_session_id, { childSessionId: e.child_session_id, agent: e.agent, description: e.description, status: "running", costUsd: 0, summary: null });
      break;
    case "subagent.ended": {
      const s = model.subagents.get(e.child_session_id);
      if (!s) return false;
      model.subagents = new Map(model.subagents).set(e.child_session_id, { ...s, status: e.status === "unknown" ? "unknown" : e.status, costUsd: e.cost.usd, summary: e.summary ?? null });
      break;
    }
    case "checkpoint.created":
      return false;
    case "branch.switched":
      if (model.info) model.info.active_leaf_id = e.active_leaf_id;
      model.needsReload = true;
      break;
    case "lease.changed":
      model.driver = e.driver ?? null;
      break;
    case "browser.action":
      model.browser = { ...model.browser, actions: [...model.browser.actions.slice(-49), e] };
      break;
    case "hook.ran":
      return false;
    case "session.error":
      model.error = { code: e.code, message: e.message, link: e.link };
      break;
    case "usage.updated":
      model.usage = e;
      break;
    case "status":
      model.status = e.state;
      model.activity = e.activity ?? null;
      if (model.info) model.info.status = e.state;
      break;
    case "browser.frame":
      model.browser = { ...model.browser, frame: e };
      break;
    case "browser.control":
      model.browser = { ...model.browser, control: e };
      break;
    default:
      // terminal.output and unknown event types (02 §10) are ignored here.
      return false;
  }
  model.rev++;
  return true;
}

/** Is a turn in flight (Send → Stop, steer by default)? */
export function isBusy(model: SessionModel): boolean {
  return model.status === "running" || model.status === "awaiting_ask" || model.status === "compacting" || model.status === "interrupting" || model.status === "paused";
}

/** Read-only when another client holds the lease (D20). */
export function drivenElsewhere(model: SessionModel, selfInstanceId: string): ClientRef | null {
  if (model.driver && model.driver.client_instance_id !== selfInstanceId) return model.driver;
  return null;
}

/** Sibling navigation for `‹ 2/3 ›` (D24). */
export function siblingInfo(model: SessionModel, messageId: string): { index: number; ids: string[] } | null {
  const ids = model.siblings[messageId];
  if (!ids || ids.length < 2) return null;
  return { index: ids.indexOf(messageId), ids };
}
