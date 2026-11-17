// In-memory engine behind the Platform adapter, for tests and for running the UI in a
// plain browser (`pnpm dev` without Tauri). It speaks the same protocol shapes as the
// daemon for the methods the UI uses; anything else fails with `method_not_found`, the
// same way an unserved method fails on the real daemon.
import type {
  Ask,
  InputPart,
  Message,
  Part,
  SessionEvent,
  SessionEventEnvelope,
  SessionInfo,
  SessionSummary,
} from "@genie/protocol";
import type { HostInfo, HostMessage, Platform, WireError } from "../types";

export interface MockOptions {
  /** Delay between streamed deltas (ms). 0 in tests. */
  tickMs?: number;
  firstRun?: boolean;
  /** Reply text; default is a Markdown sample. */
  reply?: (prompt: string) => string;
}

const SAMPLE = `Here is a quick overview.

## Steps

1. Read the input.
2. Transform it with \`map\`.
3. Write the result.

\`\`\`ts
export function double(xs: number[]): number[] {
  return xs.map((x) => x * 2);
}
\`\`\`

| Step | Cost |
|---|---|
| read | low |
| write | medium |

The area of a circle is $\\pi r^2$.`;

interface MockSession {
  info: SessionInfo;
  events: SessionEventEnvelope[];
  messages: Message[];
  subscribed: boolean;
  running?: { cancel: () => void };
}

function err(kind: string, message: string, code = -32000): WireError {
  return { code, message, data: { kind } };
}

let counter = 0;
const uid = (p: string) => `${p}_${Date.now().toString(36)}${(++counter).toString(36)}`;
const now = () => new Date().toISOString();

export function createMockPlatform(opts: MockOptions = {}): Platform & { sessions: Map<string, MockSession>; emit: (m: HostMessage) => void } {
  const tick = opts.tickMs ?? 18;
  const sessions = new Map<string, MockSession>();
  let listener: ((m: HostMessage) => void) | null = null;
  let workspaceOn = false;
  let nextReq = 1;
  const pendingAsks = new Map<number, (answer: unknown) => void>();
  let state: Record<string, unknown> = opts.firstRun ? {} : { firstRunDone: true };
  const config: Record<string, unknown> = { model: "mock/genie-small", theme: "system", "memory.enabled": true };
  const projects = [{ project_id: "prj_demo", name: "Demo project", instructions: "", starred: true }];
  const memory = [{ memory_id: "mem_1", scope: "user", text: "Prefers TypeScript examples.", updated_at: now() }];
  const providers = [
    { id: "mock", kind: "openai_chat", status: "ready", credentials: [{ credential_id: "cred_1", kind: "key", label: "Mock key", active: true, hint: "abcd" }] },
    { id: "anthropic", kind: "anthropic", status: "needs_auth", credentials: [] },
  ];
  const blobs = new Map<string, string>();
  const uploads = new Map<string, string[]>();

  const emit = (m: HostMessage) => listener?.(m);
  const notify = (method: string, params: unknown) => emit({ kind: "batch", messages: [{ method, params }] });

  function summary(s: MockSession): SessionSummary {
    const { session_id, kind, title, status, updated_at, pinned, archived, project_id, workspace_id } = s.info;
    return { session_id, kind, title, status, updated_at, pinned, archived, project_id, workspace_id };
  }

  function publish(s: MockSession, event: SessionEvent, durable = true, turn_id?: string) {
    const env: SessionEventEnvelope = {
      session_id: s.info.session_id,
      ts: now(),
      event,
      turn_id: turn_id ?? null,
      ...(durable ? { seq: ++s.info.head_seq } : { ref_seq: s.info.head_seq }),
    };
    if (durable) s.events.push(env);
    if (s.subscribed) notify("event", env);
  }

  function setStatus(s: MockSession, state: SessionInfo["status"]) {
    s.info.status = state;
    s.info.updated_at = now();
    publish(s, { type: "status", state }, false);
    if (workspaceOn) notify("session.listed", { change: "updated", session: summary(s) });
  }

  function askUser(ask: Ask): Promise<{ decision: string }> {
    const req_id = nextReq++;
    return new Promise((resolve) => {
      pendingAsks.set(req_id, (a) => resolve(a as { decision: string }));
      emit({ kind: "server_request", req_id, method: "permission.ask", params: { ask } });
    });
  }

  function runTurn(s: MockSession, prompt: string, userMsgId: string) {
    const turn_id = uid("trn");
    const text = opts.reply ? opts.reply(prompt) : SAMPLE;
    let cancelled = false;
    s.running = { cancel: () => (cancelled = true) };
    publish(s, { type: "turn.started", turn_id, message_id: userMsgId }, true, turn_id);
    setStatus(s, "running");
    const msgId = uid("msg");
    const assistant: Message = { id: msgId, role: "assistant", parts: [], created_at: now(), parent_id: userMsgId, turn_id, model: s.info.model };
    s.messages.push(assistant);
    const finish = (status: "completed" | "cancelled") => {
      s.running = undefined;
      publish(s, { type: "usage.updated", context_tokens: 1200 + text.length, context_window: 200000, cost: { usd: 0.0021 }, session: { input_tokens: 1200, output_tokens: 300 } }, false, turn_id);
      publish(s, { type: "turn.settled", turn_id, status }, true, turn_id);
      setStatus(s, "idle");
    };
    const stream = async () => {
      if (/permission|run|shell/i.test(prompt)) {
        const callPart = uid("prt");
        const call_id = uid("call");
        publish(s, { type: "part.started", kind: "tool_call", message_id: msgId, part_id: callPart }, true, turn_id);
        const part: Part = { type: "tool_call", id: callPart, call_id, name: "shell", input: { command: "ls -la" } };
        assistant.parts.push(part);
        publish(s, { type: "part.ended", part_id: callPart, part }, true, turn_id);
        publish(s, { type: "tool.started", call_id, name: "shell", resource: "ls -la" }, true, turn_id);
        const ask: Ask = { id: uid("ask"), kind: "permission", session_id: s.info.session_id, created_at: now(), tool: "shell", resource: "ls -la", preview: { type: "command", command: "ls -la", cwd: "~" }, save_suggestions: [{ rule: "Bash(ls:*)", scope: "user" }], turn_id };
        publish(s, { type: "permission.asked", ask }, true, turn_id);
        setStatus(s, "awaiting_ask");
        const answer = await askUser(ask);
        publish(s, { type: "permission.resolved", ask_id: ask.id, by: { client_instance_id: "mock", client_kind: "desktop" }, decision: (answer.decision as "once") ?? "reject" }, true, turn_id);
        setStatus(s, "running");
        const resPart = uid("prt");
        const ok = answer.decision === "once" || answer.decision === "always";
        const result: Part = { type: "tool_result", id: resPart, call_id, content: ok ? "total 0\ndrwxr-xr-x  2 me  staff  64 .\n" : "declined", is_error: !ok };
        assistant.parts.push(result);
        publish(s, { type: "tool.ended", call_id, outcome: ok ? "ok" : "declined", result_part_id: resPart }, true, turn_id);
        publish(s, { type: "part.ended", part_id: resPart, part: result }, true, turn_id);
      }
      const partId = uid("prt");
      publish(s, { type: "part.started", kind: "text", message_id: msgId, part_id: partId }, true, turn_id);
      const tokens = text.match(/\S+\s*|\s+/g) ?? [];
      for (let i = 0; i < tokens.length; i += 2) {
        if (cancelled) break;
        await new Promise((r) => setTimeout(r, tick));
        publish(s, { type: "part.delta", part_id: partId, text: tokens.slice(i, i + 2).join("") }, false, turn_id);
      }
      const full = cancelled ? text.slice(0, 40) : text;
      const part: Part = { type: "text", id: partId, text: full };
      assistant.parts.push(part);
      publish(s, { type: "part.ended", part_id: partId, part }, true, turn_id);
      if (!s.info.title) {
        s.info.title = prompt.slice(0, 40) || "New chat";
        publish(s, { type: "session.updated", title: s.info.title }, true);
      }
      finish(cancelled ? "cancelled" : "completed");
    };
    void stream();
  }

  function session(id: unknown): MockSession {
    const s = sessions.get(String(id));
    if (!s) throw err("not_found", `no session ${String(id)}`, -32004);
    return s;
  }

  const handlers: Record<string, (p: Record<string, any>) => unknown> = {
    "workspace.subscribe": () => {
      workspaceOn = true;
      setTimeout(() => notify("workspace.snapshot", { sessions: [...sessions.values()].map(summary), pending_asks: pendingAsks.size, clients: [] }), 0);
      return {};
    },
    "session.list": () => ({ items: [...sessions.values()].map(summary) }),
    "session.create": (p) => {
      const id = uid("ses");
      const s: MockSession = {
        info: { session_id: id, kind: p.kind ?? "chat", model: p.model, effort: p.effort ?? null, mode: p.mode ?? "default", head_seq: 0, status: "idle", created_at: now(), updated_at: now(), incognito: !!p.incognito, project_id: p.project_id ?? null, workspace_id: p.workspace_path ?? null, title: null },
        events: [],
        messages: [],
        subscribed: false,
      };
      sessions.set(id, s);
      publish(s, { type: "session.created", kind: s.info.kind, mode: s.info.mode, model: s.info.model });
      if (workspaceOn) notify("session.listed", { change: "created", session: summary(s) });
      return { session_id: id };
    },
    "session.get": (p) => {
      const s = session(p.session_id);
      return { session: s.info, path: s.messages, siblings: {} };
    },
    "session.subscribe": (p) => {
      const s = session(p.session_id);
      s.subscribed = true;
      const after = p.after_seq ?? 0;
      setTimeout(() => {
        for (const e of s.events) if ((e.seq ?? 0) > after) notify("event", e);
        notify("session.synced", { session_id: s.info.session_id, seq: s.info.head_seq });
      }, 0);
      return { head_seq: s.info.head_seq, status: s.info.status, driver: null };
    },
    "session.unsubscribe": (p) => {
      session(p.session_id).subscribed = false;
      return {};
    },
    "session.prompt": (p) => {
      const s = session(p.session_id);
      const id = uid("msg");
      const parts = p.parts as InputPart[];
      const text = parts.map((x) => (x.type === "text" ? x.text : "")).join("");
      s.messages.push({ id, role: "user", parts: [{ type: "text", id: uid("prt"), text }], created_at: now(), parent_id: null });
      publish(s, { type: "message.admitted", message_id: id, delivery: p.delivery, parts, role: "user", source: "user", parent_id: p.parent_id ?? null });
      if (!s.running) runTurn(s, text, id);
      return { message_id: id, inbox_id: uid("inb") };
    },
    "session.interrupt": (p) => {
      const s = session(p.session_id);
      const was = !!s.running;
      s.running?.cancel();
      return { interrupted: was };
    },
    "session.update": (p) => {
      const s = session(p.session_id);
      const { session_id: _ignored, ...changes } = p;
      Object.assign(s.info, changes);
      publish(s, { type: "session.updated", ...changes });
      if (workspaceOn) notify("session.listed", { change: "updated", session: summary(s) });
      return {};
    },
    "session.delete": (p) => {
      const s = session(p.session_id);
      sessions.delete(s.info.session_id);
      if (workspaceOn) notify("session.listed", { change: "deleted", session: summary(s) });
      return {};
    },
    "session.take_over": () => ({}),
    "session.resume": () => ({}),
    "session.regenerate": (p) => {
      const s = session(p.session_id);
      runTurn(s, "regenerate", p.message_id);
      return {};
    },
    "session.search": (p) => ({
      hits: [...sessions.values()]
        .filter((s) => (s.info.title ?? "").toLowerCase().includes(String(p.query).toLowerCase()))
        .map((s) => ({ session_id: s.info.session_id, message_id: "", snippet: s.info.title ?? "" })),
    }),
    "session.export": (p) => ({ text: `# ${session(p.session_id).info.title ?? "Chat"}\n` }),
    "permission.respond": () => ({}),
    "permission.pending": () => ({ asks: [] }),
    "project.list": () => ({ projects }),
    "project.create": (p) => {
      const project_id = uid("prj");
      projects.push({ project_id, name: p.name, instructions: p.instructions ?? "", starred: false });
      return { project_id };
    },
    "model.list": () => ({
      models: [
        { id: "mock/genie-small", name: "Genie Small", provider: "mock", context_window: 128000, effort_levels: [] },
        { id: "mock/genie-large", name: "Genie Large", provider: "mock", context_window: 200000, effort_levels: ["low", "medium", "high"] },
      ],
    }),
    "provider.list": () => ({ providers }),
    "provider.test": () => ({ ok: true, latency_ms: 120 }),
    "provider.credential.add": (p) => {
      const pr = providers.find((x) => x.id === p.provider);
      const credential_id = uid("cred");
      pr?.credentials.push({ credential_id, kind: p.kind, label: p.label, active: true, hint: String(p.secret ?? "").slice(-4) });
      if (pr) pr.status = "ready";
      return { credential_id };
    },
    "provider.credential.remove": () => ({}),
    "provider.credential.activate": () => ({}),
    "provider.login.start": () => ({ login_id: uid("login"), user_code: "ABCD-EFGH", verification_uri: "https://microsoft.com/devicelogin" }),
    "provider.login.status": () => ({ state: "pending" }),
    "catalog.status": () => ({ source: "bundled", fetched_at: now() }),
    "config.schema": () => ({
      schema: {
        type: "object",
        properties: {
          model: { type: "string", description: "Default model id for new sessions." },
          theme: { $ref: "#/$defs/Theme" },
          memory: { $ref: "#/$defs/Toggle" },
          telemetry: { type: "object", properties: { crashReports: { type: "boolean" }, usage: { type: "boolean" } } },
        },
        $defs: { Theme: { type: "string", enum: ["system", "light", "dark"] }, Toggle: { type: "object", properties: { enabled: { type: "boolean" } } } },
      },
      ui: {
        keys: {
          model: { title: "Default model", scopes_allowed: ["user", "project", "local"], restart_required: false, advanced: false },
          theme: { title: "Theme", scopes_allowed: ["user"], restart_required: false, advanced: false },
          "memory.enabled": { title: "Memory", scopes_allowed: ["user"], restart_required: false, advanced: false },
          "telemetry.crashReports": { title: "Crash reports", scopes_allowed: ["user"], restart_required: false, advanced: false },
          "telemetry.usage": { title: "Usage analytics", scopes_allowed: ["user"], restart_required: false, advanced: false },
        },
      },
    }),
    "config.get": (p) => {
      const keys: string[] = p.keys ?? Object.keys(config);
      const values: Record<string, unknown> = {};
      const sources: Record<string, string> = {};
      for (const k of keys)
        if (k in config) {
          values[k] = config[k];
          sources[k] = "user";
        }
      return { values, sources, locked: [] };
    },
    "config.set": (p) => {
      if (p.key === "model" && typeof p.value !== "string") throw err("invalid_params", "model must be a string", -32602);
      config[p.key] = p.value;
      return {};
    },
    "config.diagnostics": () => ({ problems: [] }),
    "policy.effective": () => ({ values: {}, source_per_key: {} }),
    "memory.list": () => ({ items: memory }),
    "memory.delete": (p) => {
      const i = memory.findIndex((m) => m.memory_id === p.memory_id);
      if (i >= 0) memory.splice(i, 1);
      return {};
    },
    "memory.update": () => ({}),
    "memory.set_state": () => ({}),
    "mcp.list": () => ({ servers: [{ name: "files", scope: "user", status: "connected", tools: 4 }] }),
    "skill.list": () => ({ skills: [{ id: "pdf", name: "PDF", description: "Read and fill PDFs", enabled: true, source: "builtin" }] }),
    "usage.query": () => ({ rows: [{ keys: ["mock"], usage: { input_tokens: 12000, output_tokens: 3400 }, cost: { usd: 0.12 } }] }),
    "daemon.status": () => ({ version: "0.1.0-mock", pid: 1, uptime_s: 42, socket: "mock", channel: "dev", clients: [], sessions_running: 0 }),
    "doctor.run": () => ({ checks: [{ id: "daemon", status: "ok", detail: "mock engine" }] }),
    "diagnostics.collect": () => ({ text: "mock diagnostics" }),
    "artifact.list": () => ({ artifacts: [] }),
    "browser.status": () => ({ installed: [], running: false, profile_path: "" }),
    "import.scan": () => ({ report: { items: [] } }),
    "trust.get": (p) => ({ path: p.path, state: "unknown" }),
    "trust.set": () => ({}),
    "blob.put.begin": () => {
      const id = uid("up");
      uploads.set(id, []);
      return { upload_id: id };
    },
    "blob.put.chunk": (p) => {
      uploads.get(p.upload_id)?.push(p.data_b64);
      return {};
    },
    "blob.put.end": (p) => {
      const blob_id = uid("blob");
      blobs.set(blob_id, (uploads.get(p.upload_id) ?? []).join(""));
      uploads.delete(p.upload_id);
      return { blob_id, sha256: "0".repeat(64) };
    },
  };

  return {
    kind: "mock",
    sessions,
    emit,
    async connect(onMessage) {
      listener = onMessage;
      onMessage({ kind: "status", state: "ready", engine_version: "0.1.0-mock" });
      const info: HostInfo = { client_instance_id: "desktop_mock", os: "mock", app_version: "0.1.0", batch_ms: 8 };
      return info;
    },
    async request(method, params) {
      const h = handlers[method];
      if (!h) throw err("method_not_found", `method not found: ${method}`, -32601);
      return h((params ?? {}) as Record<string, any>);
    },
    async respond(reqId, result) {
      const f = pendingAsks.get(reqId);
      pendingAsks.delete(reqId);
      f?.("ok" in result ? result.ok : { decision: "cancel" });
    },
    async retry() {},
    async openExternal(url) {
      globalThis.open?.(url, "_blank", "noopener");
    },
    async openPath() {},
    async loadState() {
      return state;
    },
    async saveState(s) {
      state = s;
    },
    async saveFile(name) {
      return `~/Downloads/${name}`;
    },
    artifactUrl: () => "about:blank",
    log() {},
  };
}
