// In-memory engine behind the Platform adapter, for tests and for running the UI in a
// plain browser (`bun run web` without Tauri). It speaks the same protocol shapes as the
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
import { azureConfig, azureModelId, azureStream, type AzureConfig, type ChatMessage } from "./azure";
import { testReply } from "./testModel";

export interface MockOptions {
  /** Delay between streamed deltas (ms). 0 in tests. */
  tickMs?: number;
  firstRun?: boolean;
  /** Reply text; default is a Markdown sample. */
  reply?: (prompt: string) => string;
  /** What the native pickers return (default: a folder / a save path). */
  pickFolder?: string | null;
  pickFiles?: string[] | null;
  /** Replay ring size of mock terminals (bytes); default 64 KiB. */
  terminalRing?: number;
}

/** A mock terminal: a line-echoing "shell" with a replay ring and absolute offsets. */
interface MockTerm {
  id: string;
  sessionId: string;
  /** Bytes as a binary string; `ring[0]` is at absolute offset `ringStart`. */
  ring: string;
  ringStart: number;
  line: string;
  attached: boolean;
}

const PROMPT = "mock$ ";
const toBin = (text: string) => String.fromCharCode(...new TextEncoder().encode(text));

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

export function createMockPlatform(opts: MockOptions = {}): Platform & { sessions: Map<string, MockSession>; emit: (m: HostMessage) => void; notifications: { title: string; body?: string }[] } {
  const notifications: { title: string; body?: string }[] = [];
  const tick = opts.tickMs ?? 18;
  const sessions = new Map<string, MockSession>();
  let listener: ((m: HostMessage) => void) | null = null;
  let workspaceOn = false;
  let nextReq = 1;
  const pendingAsks = new Map<number, (answer: unknown) => void>();
  let state: Record<string, unknown> = opts.firstRun ? {} : { firstRunDone: true };
  const config: Record<string, unknown> = { model: azureConfig ? azureModelId(azureConfig) : "mock/genie-small", theme: "system", "memory.enabled": true };
  const projects = [{ project_id: "prj_demo", name: "Demo project", instructions: "", starred: true }];
  const memory = [{ memory_id: "mem_1", scope: "user", text: "Prefers TypeScript examples.", updated_at: now() }];
  const providers: { id: string; kind: string; status: string; credentials: { credential_id: string; kind: string; label: string; active: boolean; hint: string }[]; endpoint?: string }[] = [
    { id: "mock", kind: "openai_chat", status: "ready", credentials: [{ credential_id: "cred_1", kind: "key", label: "Mock key", active: true, hint: "abcd" }] },
    { id: "anthropic", kind: "anthropic", status: "needs_auth", credentials: [] },
    { id: "openai", kind: "openai", status: "needs_auth", credentials: [] },
    { id: "google", kind: "google", status: "needs_auth", credentials: [] },
    azureConfig
      ? { id: "azure-foundry", kind: "azure-foundry", status: "ready", credentials: [{ credential_id: "cred_azure", kind: "key", label: "Azure key (.env.local)", active: true, hint: "env" }], endpoint: "(set in .env.local)" }
      : { id: "azure-foundry", kind: "azure-foundry", status: "needs_auth", credentials: [] },
    { id: "openrouter", kind: "openrouter", status: "needs_auth", credentials: [] },
    { id: "ollama", kind: "ollama", status: "ready", credentials: [], endpoint: "http://localhost:11434" },
  ];
  const blobs = new Map<string, string>();
  const terms = new Map<string, MockTerm>();
  const trusted = new Map<string, { path: string; decision: "trust" | "deny"; decided_at: string; approvals: number }>([["/tmp/mock-repo", { path: "/tmp/mock-repo", decision: "trust", decided_at: now(), approvals: 1 }]]);
  const rules: { rule: string; class: "allow" | "ask" | "deny"; scope: "user" | "managed"; source: string; locked: boolean }[] = [
    { rule: "Bash(ls:*)", class: "allow", scope: "user", source: "settings.json", locked: false },
    { rule: "WebFetch(domain:evil.example)", class: "deny", scope: "managed", source: "policy", locked: true },
  ];
  let deleteToken: string | null = null;
  const projectFiles: { project_id: string; file_id: string; name: string; blob_id: string; size: number; tokens: number; added_at: string }[] = [];
  const ringCap = opts.terminalRing ?? 64 * 1024;
  const uploads = new Map<string, string[]>();

  const emit = (m: HostMessage) => listener?.(m);
  const notify = (method: string, params: unknown) => emit({ kind: "batch", messages: [{ method, params }] });
  const devServers = new Map<string, { name: string; state: string; url: string; port: number; command: string; pid: number }[]>();
  const noVerify = new Set<string>();
  const devList = (sid: unknown) => (devServers.get(String(sid)) ?? []).map((x) => ({ ...x }));
  const startDev = async (sid: string, name: string) => {
    const list = devList(sid).filter((x) => x.name !== name);
    const srv = { name, state: "starting", url: "http://localhost:5173", port: 5173, command: "npm run dev", pid: 4242 };
    devServers.set(sid, [...list, srv]);
    notify("preview.changed", { session_id: sid, servers: devList(sid) });
    await new Promise((r) => setTimeout(r, 600));
    srv.state = "running";
    notify("preview.changed", { session_id: sid, servers: devList(sid) });
    return { ...srv };
  };

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

  /** A turn answered by the real Azure OpenAI deployment (dev preview with `.env.local`). */
  function runAzureTurn(s: MockSession, cfg: AzureConfig, userMsgId: string) {
    const turn_id = uid("trn");
    const ctrl = new AbortController();
    s.running = { cancel: () => ctrl.abort() };
    publish(s, { type: "turn.started", turn_id, message_id: userMsgId }, true, turn_id);
    setStatus(s, "running");

    // Conversation so far, up to and including the prompting user message.
    const upto = s.messages.findIndex((m) => m.id === userMsgId);
    const history: ChatMessage[] = s.messages
      .slice(0, upto < 0 ? undefined : upto + 1)
      .map((m) => ({ role: m.role === "assistant" ? ("assistant" as const) : ("user" as const), content: m.parts.map((p) => (p.type === "text" ? p.text : "")).join("") }))
      .filter((m) => m.content.trim());

    const msgId = uid("msg");
    const assistant: Message = { id: msgId, role: "assistant", parts: [], created_at: now(), parent_id: userMsgId, turn_id, model: s.info.model };
    s.messages.push(assistant);
    const partId = uid("prt");
    publish(s, { type: "part.started", kind: "text", message_id: msgId, part_id: partId }, true, turn_id);

    let text = "";
    void (async () => {
      let status: "completed" | "cancelled" | "failed" = "completed";
      let error: { code: string; message: string } | undefined;
      let usage = { input_tokens: 0, output_tokens: 0 };
      try {
        const u = await azureStream(cfg, history, (d) => {
          text += d;
          publish(s, { type: "part.delta", part_id: partId, text: d }, false, turn_id);
        }, ctrl.signal);
        // Rough estimate when the API version doesn't report usage while streaming.
        usage = u ?? { input_tokens: Math.ceil(history.reduce((n, m) => n + m.content.length, 0) / 4), output_tokens: Math.ceil(text.length / 4) };
      } catch (e) {
        if (ctrl.signal.aborted) status = "cancelled";
        else {
          status = "failed";
          error = { code: "provider_error", message: e instanceof Error ? e.message : String(e) };
        }
      }
      const part: Part = { type: "text", id: partId, text };
      assistant.parts.push(part);
      assistant.usage = usage;
      publish(s, { type: "part.ended", part_id: partId, part }, true, turn_id);
      if (!s.info.title) {
        const first = history.find((m) => m.role === "user")?.content ?? "";
        s.info.title = first.slice(0, 40) || "New chat";
        publish(s, { type: "session.updated", title: s.info.title }, true);
      }
      s.running = undefined;
      publish(s, { type: "step.ended", finish: status === "completed" ? "stop" : status === "failed" ? "error" : "cancelled", usage, cost: { usd: 0 } }, true, turn_id);
      publish(s, { type: "usage.updated", context_tokens: usage.input_tokens + usage.output_tokens, context_window: 400000, cost: { usd: 0 }, session: usage }, false, turn_id);
      publish(s, { type: "turn.settled", turn_id, status, ...(error ? { error } : {}) }, true, turn_id);
      setStatus(s, "idle");
    })();
  }

  function runTurn(s: MockSession, prompt: string, userMsgId: string) {
    if (!opts.reply && azureConfig && s.info.model === azureModelId(azureConfig)) return runAzureTurn(s, azureConfig, userMsgId);
    const turn_id = uid("trn");
    const scripted = !opts.reply && s.info.model === "mock/genie-test" ? testReply(prompt) : null;
    const text = opts.reply ? opts.reply(prompt) : (scripted?.text ?? SAMPLE);
    let cancelled = false;
    s.running = { cancel: () => (cancelled = true) };
    publish(s, { type: "turn.started", turn_id, message_id: userMsgId }, true, turn_id);
    setStatus(s, "running");
    const msgId = uid("msg");
    const assistant: Message = { id: msgId, role: "assistant", parts: [], created_at: now(), parent_id: userMsgId, turn_id, model: s.info.model, usage: { input_tokens: 1200, output_tokens: 300 }, cost: { usd: 0.0021 } };
    s.messages.push(assistant);
    const finish = (status: "completed" | "cancelled" | "failed") => {
      s.running = undefined;
      publish(s, { type: "step.ended", finish: status === "completed" ? "stop" : status === "failed" ? "error" : "cancelled", usage: { input_tokens: 1200, output_tokens: 300 }, cost: { usd: 0.0021 } }, true, turn_id);
      publish(s, { type: "usage.updated", context_tokens: 1200 + text.length, context_window: 200000, cost: { usd: 0.0021 }, session: { input_tokens: 1200, output_tokens: 300 } }, false, turn_id);
      const error = status === "failed" && scripted?.error ? { code: "provider_error", message: scripted.error } : undefined;
      publish(s, { type: "turn.settled", turn_id, status, ...(error ? { error } : {}) }, true, turn_id);
      setStatus(s, "idle");
    };
    const stream = async () => {
      if (/dev server|preview/i.test(prompt) && s.info.kind === "code") {
        const callPart = uid("prt");
        const call_id = uid("call");
        const input = { name: "web", runtimeExecutable: "npm", runtimeArgs: ["run", "dev"], port: 5173 };
        const part: Part = { type: "tool_call", id: callPart, call_id, name: "preview_start", input };
        publish(s, { type: "part.started", kind: "tool_call", message_id: msgId, part_id: callPart }, true, turn_id);
        assistant.parts.push(part);
        publish(s, { type: "part.ended", part_id: callPart, part }, true, turn_id);
        publish(s, { type: "tool.started", call_id, name: "preview_start", resource: "Bash(npm run dev)" }, true, turn_id);
        const srv = await startDev(s.info.session_id, "web");
        const resPart = uid("prt");
        const result: Part = { type: "tool_result", id: resPart, call_id, content: `\`web\`: running at ${srv.url} (npm run dev), pid 4242. Saved to .genie/launch.json.`, is_error: false };
        assistant.parts.push(result);
        publish(s, { type: "tool.ended", call_id, outcome: "ok", result_part_id: resPart }, true, turn_id);
        publish(s, { type: "part.ended", part_id: resPart, part: result }, true, turn_id);
      } else if (/permission|run|shell/i.test(prompt)) {

        const callPart = uid("prt");
        const call_id = uid("call");
        publish(s, { type: "part.started", kind: "tool_call", message_id: msgId, part_id: callPart }, true, turn_id);
        const command = 'ps aux | egrep -i "vite|tauri" | head -20';
        const cwd = s.info.workspace_id || "/Users/me/projects/app";
        const part: Part = { type: "tool_call", id: callPart, call_id, name: "shell", input: { command } };
        assistant.parts.push(part);
        publish(s, { type: "part.ended", part_id: callPart, part }, true, turn_id);
        publish(s, { type: "tool.started", call_id, name: "shell", resource: `Bash(${command})` }, true, turn_id);
        const scope = s.info.kind === "code" ? "local" : "user";
        const ask: Ask = { id: uid("ask"), kind: "permission", session_id: s.info.session_id, created_at: now(), tool: "shell", resource: `Bash(${command})`, preview: { type: "command", command, cwd }, save_suggestions: [{ rule: "Bash(ps:*)", scope }, { rule: "Bash(egrep:*)", scope }], turn_id };
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
      if (scripted?.thinking && !cancelled) {
        const rId = uid("prt");
        publish(s, { type: "part.started", kind: "reasoning", message_id: msgId, part_id: rId }, true, turn_id);
        for (const chunk of scripted.thinking.match(/\S+\s*/g) ?? []) {
          if (cancelled) break;
          await new Promise((r) => setTimeout(r, tick * 2));
          publish(s, { type: "part.delta", part_id: rId, text: chunk }, false, turn_id);
        }
        const rPart: Part = { type: "reasoning", id: rId, text: scripted.thinking };
        assistant.parts.push(rPart);
        publish(s, { type: "part.ended", part_id: rId, part: rPart }, true, turn_id);
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
      finish(cancelled ? "cancelled" : scripted?.error ? "failed" : "completed");
    };
    void stream();
  }

  function termWrite(t: MockTerm, text: string) {
    const bin = toBin(text);
    const start = t.ringStart + t.ring.length;
    t.ring += bin;
    if (t.ring.length > ringCap) {
      const drop = t.ring.length - ringCap;
      t.ring = t.ring.slice(drop);
      t.ringStart += drop;
    }
    if (t.attached) {
      notify("event", { session_id: t.sessionId, ts: now(), event: { type: "terminal.output", terminal_id: t.id, start, end: start + bin.length, data_b64: btoa(bin) } });
    }
  }

  function termExit(t: MockTerm, code: number | null) {
    terms.delete(t.id);
    notify("terminal.exited", { terminal_id: t.id, code });
  }

  function termInput(t: MockTerm, bin: string) {
    const text = new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0)));
    for (const ch of text) {
      if (ch === "\r") {
        const line = t.line.trim();
        t.line = "";
        if (line === "exit" || line.startsWith("exit ")) {
          termWrite(t, "\r\nexit\r\n");
          termExit(t, Number(line.slice(5)) || 0);
          return;
        }
        const out = !line ? "" : line.startsWith("echo ") ? `${line.slice(5)}\r\n` : `mock: command not found: ${line.split(" ")[0]}\r\n`;
        termWrite(t, `\r\n${out}${PROMPT}`);
      } else if (ch === "\x7f") {
        if (t.line) {
          t.line = t.line.slice(0, -1);
          termWrite(t, "\b \b");
        }
      } else {
        t.line += ch;
        termWrite(t, ch);
      }
    }
  }

  function term(id: unknown): MockTerm {
    const t = terms.get(String(id));
    if (!t) throw err("not_found", "terminal not found", -32004);
    return t;
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
        info: { session_id: id, kind: p.kind ?? "chat", model: p.model, effort: p.effort ?? null, mode: p.mode ?? "default", head_seq: 0, status: "idle", created_at: now(), updated_at: now(), incognito: !!p.incognito, project_id: p.project_id ?? null, workspace_id: p.workspace_path ?? null, title: null, web_search: p.web_search ?? true, code_execution: p.code_execution ?? false },
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
      // `client` built-ins never reach the model (06 §9.1, SA-48).
      const first = parts[0];
      const typed = first?.type === "command" ? first.name : first?.type === "text" ? /^\/(\S+)/.exec(first.text.trimStart())?.[1] : undefined;
      const builtin = typed ? MOCK_BUILTINS.find((b) => b.name === typed || b.aliases?.includes(typed)) : undefined;
      if (builtin && builtin.route === "client") throw err("command_not_prompt", `/${builtin.name} is a client command; run it in the client`, -32014);
      const text = parts.map((x) => (x.type === "text" ? x.text : x.type === "command" ? `/${x.name} ${x.args}`.trim() : "")).join("");
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
    "session.background_tool": () => ({ backgrounded: false }),
    "session.update": (p) => {
      const s = session(p.session_id);
      const { session_id: _ignored, ...changes } = p;
      if ("project_id" in changes) {
        if (s.info.kind !== "chat") throw err("invalid_params", "only Chat sessions belong to projects", -32602);
        if (changes.project_id && !projects.some((x) => x.project_id === changes.project_id)) throw err("not_found", "no such project", -32004);
        changes.project_id = changes.project_id || null;
      }
      Object.assign(s.info, changes);
      publish(s, { type: "session.updated", ...changes });
      if (workspaceOn) notify("session.listed", { change: "updated", session: summary(s) });
      return {};
    },
    "session.delete": (p) => {
      const s = session(p.session_id);
      sessions.delete(s.info.session_id);
      for (const t of [...terms.values()]) if (t.sessionId === s.info.session_id) termExit(t, null);
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
      // Like the daemon: the catalog lists every provider's models, connected or not.
      models: [
        { id: "anthropic/claude-sonnet-4-5", name: "Claude Sonnet 4.5", provider: "anthropic", context_window: 200000, effort_levels: ["low", "medium", "high"] },
        { id: "openai/gpt-5.2", name: "GPT-5.2", provider: "openai", context_window: 400000, effort_levels: ["low", "medium", "high"] },
        { id: "mock/genie-small", name: "Genie Small", provider: "mock", context_window: 128000, effort_levels: [] },
        { id: "mock/genie-large", name: "Genie Large", provider: "mock", context_window: 200000, effort_levels: ["low", "medium", "high"] },
        { id: "mock/genie-test", name: "Genie Test", provider: "mock", context_window: 200000, effort_levels: [] },
        ...(azureConfig ? [{ id: azureModelId(azureConfig), name: "GPT-5.2 (Azure)", provider: "azure-foundry", context_window: 400000, effort_levels: [] }] : []),
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
          providers: { type: "object", additionalProperties: true },
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
          providers: { title: "Providers", scopes_allowed: ["user"], restart_required: false, advanced: false },
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
      // `providers.<id>` writes land inside the `providers` object, as in the daemon.
      const [top, ...rest] = String(p.key).split(".");
      if (top === "providers" && rest.length) config.providers = { ...((config.providers as Record<string, unknown>) ?? {}), [rest.join(".")]: p.value };
      else config[p.key] = p.value;
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
    "mcp.resources": (p) => ({ items: p.name === "files" ? [{ uri: "file:///notes/todo.md", name: "todo.md", mimeType: "text/markdown" }, { uriTemplate: "file:///{path}", name: "any file" }] : [] }),
    "mcp.prompts": (p) => ({ items: p.name === "files" ? [{ name: "summarize", description: "Summarize a file", arguments: [{ name: "path", required: true }, { name: "style" }] }] : [] }),
    "mcp.resource.read": (p) => {
      const blob_id = uid("blob");
      blobs.set(blob_id, btoa(`resource ${p.uri}`));
      return { blob_id, mime: "text/markdown" };
    },
    "command.list": (p) => {
      const kind = p.session_id ? session(p.session_id).info.kind : (p.kind ?? "chat");
      return {
        commands: [
          ...MOCK_BUILTINS.filter((b) => kind === "code" || !b.codeOnly).map(({ codeOnly: _c, ...b }) => b),
          { name: "review", description: "Review the current changes", args_hint: "[focus]", source: "user", route: "prompt", immediate: false },
          { name: "pdf", description: "Read and fill PDFs", source: "skill", route: "prompt", immediate: false },
          { name: "files:summarize", description: "Summarize a file", args_hint: "<path> [style]", source: "mcp_prompt", route: "prompt", immediate: false, server: "files" },
        ],
        diagnostics: [],
      };
    },
    "session.compact": () => ({}),
    "skill.list": () => ({ skills: [{ id: "pdf", name: "PDF", description: "Read and fill PDFs", enabled: true, source: "builtin" }] }),
    "usage.query": () => ({ rows: [{ keys: ["mock"], usage: { input_tokens: 12000, output_tokens: 3400 }, cost: { usd: 0.12 } }] }),
    "daemon.status": () => ({ version: "0.1.0-mock", pid: 1, uptime_s: 42, socket: "mock", channel: "dev", clients: [], sessions_running: 0 }),
    "doctor.run": () => ({ checks: [{ id: "daemon", status: "ok", detail: "mock engine" }] }),
    "diagnostics.collect": () => ({ text: "mock diagnostics" }),
    "artifact.list": () => ({ artifacts: [] }),
    "browser.status": () => ({ installed: [], running: false, profile_path: "" }),
    "import.scan": () => ({ report: { items: [] } }),
    "trust.get": (p) => ({ path: p.path, state: trusted.has(p.path) ? "trusted" : "unknown" }),
    "trust.set": (p) => {
      if (p.decision === "revoke") trusted.delete(p.path);
      else trusted.set(p.path, { path: p.path, decision: p.decision === "deny" ? "deny" : "trust", decided_at: now(), approvals: 0 });
      return {};
    },
    "trust.list": () => ({ items: [...trusted.values()] }),
    "permissions.rules.list": () => ({ rules }),
    "permissions.rules.remove": (p) => {
      const i = rules.findIndex((r) => r.rule === p.rule && r.class === p.class && r.scope === p.scope);
      if (i < 0) throw err("not_found", "no such rule", -32004);
      if (rules[i].locked) throw err("locked", "managed by policy", -32011);
      rules.splice(i, 1);
      return {};
    },
    "data.delete_all.begin": () => {
      deleteToken = uid("tok");
      return { token: deleteToken, expires_in_s: 120, sessions: sessions.size, projects: projects.length, memories: memory.length, artifacts: 0, blob_bytes: 2048, clears_audit: true };
    },
    "data.delete_all.confirm": (p) => {
      if (!deleteToken || p.token !== deleteToken) throw err("invalid_params", "token expired", -32602);
      deleteToken = null;
      const n = sessions.size;
      for (const s of [...sessions.values()]) {
        sessions.delete(s.info.session_id);
        for (const t of [...terms.values()]) if (t.sessionId === s.info.session_id) termExit(t, null);
      }
      projects.splice(0);
      memory.splice(0);
      return { sessions_deleted: n, audit_cleared: true };
    },
    "project.file.list": (p) => ({ files: projectFiles.filter((f) => f.project_id === p.project_id).map(({ project_id: _p, ...f }) => f) }),
    "project.file.add": (p) => {
      const file_id = uid("pf");
      projectFiles.push({ project_id: p.project_id, file_id, name: p.name, blob_id: p.blob_id, size: 100, tokens: 25, added_at: now() });
      return { file_id };
    },
    "project.file.remove": (p) => {
      const i = projectFiles.findIndex((f) => f.file_id === p.file_id);
      if (i >= 0) projectFiles.splice(i, 1);
      return {};
    },
    // Dev servers (ticket 47): one `web` entry per Code session; starting takes ~0.6 s.
    "preview.list": (p) => ({ servers: devList(p.session_id), configs: ["web"], auto_verify: !noVerify.has(String(p.session_id)) }),
    "preview.auto_verify": (p) => {
      if (p.enabled) noVerify.delete(String(p.session_id));
      else noVerify.add(String(p.session_id));
      return {};
    },
    "preview.start": (p) => startDev(String(p.session_id), String(p.name)),
    "preview.stop": (p) => {
      const sid = String(p.session_id);
      devServers.set(sid, p.name ? devList(sid).filter((x) => x.name !== p.name) : []);
      notify("preview.changed", { session_id: sid, servers: devList(sid) });
      return {};
    },
    "preview.logs": (p) => ({ text: devList(p.session_id).some((x) => x.name === p.name) ? "  VITE v5.4.0  ready in 312 ms\n\n  ➜  Local:   http://localhost:5173/" : "" }),
    "terminal.open": (p) => {
      const s = session(p.session_id);
      if (s.info.kind !== "code") throw err("invalid_state", "terminals belong to Code sessions", -32010);
      const t: MockTerm = { id: uid("term"), sessionId: s.info.session_id, ring: "", ringStart: 0, line: "", attached: false };
      terms.set(t.id, t);
      termWrite(t, PROMPT);
      return { terminal_id: t.id };
    },
    "terminal.attach": (p) => {
      const t = term(p.terminal_id);
      t.attached = true;
      const offset = Number(p.offset ?? 0);
      const end = t.ringStart + t.ring.length;
      if (offset < t.ringStart) {
        const lastLine = t.ring.slice(t.ring.lastIndexOf("\n") + 1);
        return { truncated: true, checkpoint_b64: btoa(lastLine), replay_b64: "", end_offset: end };
      }
      return { truncated: false, checkpoint_b64: null, replay_b64: btoa(t.ring.slice(Math.min(offset, end) - t.ringStart)), end_offset: end };
    },
    "terminal.input": (p) => {
      termInput(term(p.terminal_id), atob(String(p.data_b64 ?? "")));
      return {};
    },
    "terminal.resize": (p) => {
      term(p.terminal_id);
      return {};
    },
    "terminal.close": (p) => {
      termExit(term(p.terminal_id), null);
      return {};
    },
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
    notifications,
    async connect(onMessage) {
      listener = onMessage;
      onMessage({ kind: "status", state: "ready", engine_version: "0.1.0-mock" });
      const info: HostInfo = { client_instance_id: "desktop_mock", os: "mock", app_version: "0.1.0", batch_ms: 8, user_name: "Genie" };
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
    async pickFolder() {
      return opts.pickFolder === undefined ? "/tmp/mock-repo" : opts.pickFolder;
    },
    async pickSavePath(name) {
      return `/tmp/${name}`;
    },
    async pickFiles() {
      return opts.pickFiles === undefined ? ["/tmp/mock-file.txt"] : opts.pickFiles;
    },
    async notify(title, body) {
      notifications.push({ title, body });
    },
    async setWindowTheme() {},
    async installCli() {
      return { path: "~/.local/bin/genie", target: "/Applications/Genie.app/Contents/MacOS/genie", dir: "~/.local/bin", on_path: false };
    },
    async notices() {
      return "# Third-party notices\n\n- **OpenCode** (MIT)\n";
    },
    artifactUrl: () => "about:blank",
    log() {},
  };
}

/** The engine's built-ins (06 §9.1; `genie-skills` `BUILTINS`), for the mock `command.list`. */
const MOCK_BUILTINS: { name: string; aliases?: string[]; description: string; args_hint?: string; source: "builtin"; route: "client" | "prompt"; immediate: boolean; codeOnly?: boolean }[] = [
  { name: "help", description: "Show commands and keyboard shortcuts", source: "builtin", route: "client", immediate: false },
  { name: "model", description: "Switch the model", args_hint: "[model]", source: "builtin", route: "client", immediate: false },
  { name: "effort", description: "Change reasoning effort", args_hint: "[level]", source: "builtin", route: "client", immediate: false },
  { name: "mcp", aliases: ["connectors"], description: "Manage connectors (MCP servers)", args_hint: "[enable|disable <server>]", source: "builtin", route: "client", immediate: true },
  { name: "skills", description: "Browse and manage skills", source: "builtin", route: "client", immediate: true },
  { name: "memory", description: "View and edit memory", source: "builtin", route: "client", immediate: true },
  { name: "permissions", description: "Capabilities and permission rules", source: "builtin", route: "client", immediate: true },
  { name: "cost", aliases: ["usage"], description: "Show usage and cost for this session", source: "builtin", route: "client", immediate: false },
  { name: "context", description: "Show context window usage", source: "builtin", route: "client", immediate: false },
  { name: "resume", description: "Open another chat or session", source: "builtin", route: "client", immediate: false },
  { name: "export", description: "Export this conversation", args_hint: "[markdown|json]", source: "builtin", route: "client", immediate: false },
  { name: "clear", description: "Start a new chat or session", source: "builtin", route: "client", immediate: false },
  { name: "mode", description: "Change the permission mode", args_hint: "[default|accept-edits|plan|bypass]", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "plan", description: "Switch to plan mode", args_hint: "[<description>]", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "compact", description: "Summarise the conversation to free context", args_hint: "[focus]", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "rewind", description: "Rewind files or conversation to a checkpoint", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "worktree", description: "Start a new session in a worktree", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "agents", description: "Pick an agent", source: "builtin", route: "client", immediate: false, codeOnly: true },
  { name: "init", description: "Create GENIE.md for this project", source: "builtin", route: "prompt", immediate: false, codeOnly: true },
];
