// The app controller: host connection, workspace feed, open sessions, daemon → client
// requests and UI state persisted to state.json (D23). React reads it through hooks.
import {
  GenieClient,
  type Ask,
  type AskAnswer,
  type AskParams,
  type ClientRef,
  type LeaseLost,
  type ModelInfo,
  type ProjectInfo,
  type ProviderInfo,
  type SessionEventEnvelope,
  type SessionKind,
  type SessionListed,
  type SessionSummary,
  type SessionSynced,
  type UserNotification,
  type WorkspaceSnapshot,
} from "@genie/protocol";
import { DEFAULT_APPEARANCE, type Appearance } from "@genie/ui";
import { errorKind, errorMessage, type HostInfo, type HostMessage, type HostStatus, type Platform } from "../platform";
import { Emitter } from "./emitter";
import { SessionStore } from "./session";

export type View = "main" | "settings" | "project";
export type PanelTab = "artifact" | "browser" | "changes" | "files" | "terminal";

export interface UiState {
  mode: SessionKind;
  view: View;
  activeSession: string | null;
  activeProject: string | null;
  settingsSection: string;
  settingsItem: string | null;
  panelOpen: boolean;
  panelTab: PanelTab;
  panelWidth: number;
  sidebarWidth: number;
  incognito: boolean;
  appearance: Appearance;
  firstRunDone: boolean;
  /** Last model/effort picked in the composer. */
  model: string | null;
  effort: string | null;
  recentWorkspaces: string[];
}

const DEFAULT_UI: UiState = {
  mode: "chat",
  view: "main",
  activeSession: null,
  activeProject: null,
  settingsSection: "general",
  settingsItem: null,
  panelOpen: false,
  panelTab: "artifact",
  panelWidth: 420,
  sidebarWidth: 272,
  incognito: false,
  appearance: DEFAULT_APPEARANCE,
  firstRunDone: false,
  model: null,
  effort: null,
  recentWorkspaces: [],
};

export interface Toast {
  id: number;
  title: string;
  body?: string;
  tone: "info" | "danger" | "success";
  link?: string | null;
}

type Listener<T> = (v: T) => void;

export class App extends Emitter {
  readonly client: GenieClient;
  status: HostStatus = { state: "connecting" };
  info: HostInfo | null = null;
  ui: UiState = { ...DEFAULT_UI };
  stateLoaded = false;

  /** Workspace feed (02 §3.2). */
  sessions = new Map<string, SessionSummary>();
  pendingAsks = 0;
  clients: ClientRef[] = [];
  projects: ProjectInfo[] = [];
  models: ModelInfo[] = [];
  providers: ProviderInfo[] = [];
  toasts: Toast[] = [];

  private open = new Map<string, SessionStore>();
  /** server request id → ask id, for asks the daemon asked us directly. */
  private askReqs = new Map<string, number>();
  private reqAsks = new Map<number, string>();
  private everReady = false;
  private toastId = 0;
  private configListeners = new Set<Listener<string[]>>();
  private saveTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(readonly platform: Platform) {
    super();
    this.client = new GenieClient({ request: (m, p) => platform.request(m, p) });
  }

  async start() {
    const [state] = await Promise.all([this.platform.loadState().catch(() => ({}))]);
    this.ui = { ...DEFAULT_UI, ...(state as Partial<UiState>), view: "main", appearance: { ...DEFAULT_APPEARANCE, ...((state as Partial<UiState>).appearance ?? {}) } };
    if ((state as { firstRunDone?: boolean }).firstRunDone) this.ui.firstRunDone = true;
    this.stateLoaded = true;
    this.info = await this.platform.connect((m) => this.onHost(m));
    this.changedNow();
  }

  // ---------- UI state ----------

  setUi(patch: Partial<UiState>) {
    this.ui = { ...this.ui, ...patch };
    this.changedNow();
    if (this.saveTimer) clearTimeout(this.saveTimer);
    this.saveTimer = setTimeout(() => {
      const { view: _v, ...persist } = this.ui;
      void this.platform.saveState(persist as unknown as Record<string, unknown>).catch(() => {});
    }, 300);
  }

  /** `genie://session/<id>[#part]`, `genie://settings/<section>[/<item>]` (D34, D42). */
  openDeepLink(link: string) {
    const m = /^genie:\/\/(session|settings)\/([^/#]+)(?:[/#](.+))?$/.exec(link);
    if (!m) return false;
    if (m[1] === "session") this.selectSession(m[2]);
    else this.setUi({ view: "settings", settingsSection: m[2], settingsItem: m[3] ?? null });
    return true;
  }

  selectSession(id: string | null) {
    const s = id ? this.sessions.get(id) : null;
    this.setUi({ activeSession: id, view: "main", ...(s && s.kind !== "unknown" ? { mode: s.kind } : {}) });
  }

  // ---------- host messages ----------

  private onHost(m: HostMessage) {
    switch (m.kind) {
      case "status": {
        const { kind: _k, ...status } = m;
        this.status = status;
        if ((status.state === "ready" || status.state === "degraded") && !status.locked) void this.onReady();
        this.changedNow();
        break;
      }
      case "batch":
        for (const n of m.messages) this.onNotification(n.method, n.params);
        break;
      case "server_request":
        this.onServerRequest(m.req_id, m.method, m.params);
        break;
      case "request_cancelled": {
        const askId = this.reqAsks.get(m.req_id);
        this.reqAsks.delete(m.req_id);
        if (askId) this.askReqs.delete(askId);
        break;
      }
      case "reload":
        void this.open.get(m.session_id)?.reload();
        break;
    }
  }

  private async onReady() {
    if (this.everReady) {
      // The relay resubscribed sessions and the workspace feed itself (02 §3.1).
      void this.refreshLists();
      return;
    }
    this.everReady = true;
    try {
      await this.client.workspace.subscribe({});
    } catch (e) {
      this.platform.log("warn", `workspace.subscribe: ${errorMessage(e)}`);
    }
    await this.refreshLists();
    // Re-open sessions that were open before a webview reload.
    for (const s of this.open.values()) void s.open();
  }

  async refreshLists() {
    const [projects, models, providers] = await Promise.allSettled([this.client.project.list({}), this.client.model.list({}), this.client.provider.list({})]);
    if (projects.status === "fulfilled") this.projects = projects.value.projects;
    if (models.status === "fulfilled") this.models = models.value.models;
    if (providers.status === "fulfilled") this.providers = providers.value.providers;
    this.changed();
  }

  private onNotification(method: string, params: unknown) {
    switch (method) {
      case "event": {
        const env = params as SessionEventEnvelope;
        this.open.get(env.session_id)?.apply(env);
        return;
      }
      case "session.synced":
        this.open.get((params as SessionSynced).session_id)?.markSynced();
        return;
      case "lease.lost": {
        const p = params as LeaseLost;
        this.open.get(p.session_id)?.onLeaseLost(p.new_driver ?? null);
        return;
      }
      case "workspace.snapshot": {
        const p = params as WorkspaceSnapshot;
        this.sessions = new Map(p.sessions.map((s) => [s.session_id, s]));
        this.pendingAsks = p.pending_asks;
        this.clients = p.clients;
        this.changed();
        return;
      }
      case "session.listed": {
        const p = params as SessionListed;
        this.sessions = new Map(this.sessions);
        if (p.change === "deleted") this.sessions.delete(p.session.session_id);
        else this.sessions.set(p.session.session_id, p.session);
        this.changed();
        return;
      }
      case "config.changed":
      case "policy.changed": {
        const keys = (params as { keys?: string[] }).keys ?? [];
        this.configListeners.forEach((f) => f(keys));
        return;
      }
      case "catalog.updated":
      case "provider.status":
        void this.refreshLists();
        return;
      case "notification": {
        const n = params as UserNotification;
        this.toast({ title: n.title, body: n.body, tone: "info", link: n.link });
        return;
      }
      case "daemon.restarting":
        this.toast({ title: "Engine is restarting", tone: "info" });
        return;
      default:
        return;
    }
  }

  private onServerRequest(reqId: number, method: string, params: unknown) {
    if (method !== "permission.ask") {
      void this.platform.respond(reqId, { error: { code: -32601, message: `method not found: ${method}`, data: { kind: "method_not_found" } } });
      return;
    }
    const ask = (params as AskParams).ask;
    if (!ask?.id) return;
    this.askReqs.set(ask.id, reqId);
    this.reqAsks.set(reqId, ask.id);
    const s = this.open.get(ask.session_id);
    if (s) s.addAsk(ask);
    else this.toast({ title: "Genie needs your approval", body: ask.tool ?? ask.kind, tone: "info", link: `genie://session/${ask.session_id}` });
  }

  /** Answer an ask: as the reply to the daemon's request if it asked us, else `permission.respond`. */
  async answerAsk(ask: Ask, answer: AskAnswer) {
    const reqId = this.askReqs.get(ask.id);
    if (reqId != null) {
      this.askReqs.delete(ask.id);
      this.reqAsks.delete(reqId);
      try {
        await this.platform.respond(reqId, { ok: answer });
        this.open.get(ask.session_id)?.dropAsk(ask.id);
        return;
      } catch {
        /* the request went stale (reconnect): fall through to permission.respond */
      }
    }
    await this.client.permission.respond({ ask_id: ask.id, ...answer });
    this.open.get(ask.session_id)?.dropAsk(ask.id);
  }

  // ---------- sessions ----------

  /** The store for an open session (subscribes on first use). */
  session(id: string): SessionStore {
    let s = this.open.get(id);
    if (!s) {
      s = new SessionStore(this, id);
      this.open.set(id, s);
      if (this.status.state === "ready" || this.status.state === "degraded") void s.open();
    }
    return s;
  }

  closeSession(id: string) {
    const s = this.open.get(id);
    this.open.delete(id);
    void s?.close();
  }

  /** Model for new sessions: last picked, else the configured default, else the first listed. */
  async defaultModel(): Promise<string | null> {
    if (this.ui.model) return this.ui.model;
    try {
      const r = await this.client.config.get({ keys: ["model"] });
      const m = r.values.model;
      if (typeof m === "string" && m) return m;
    } catch {
      /* fall through */
    }
    return this.models[0]?.id ?? null;
  }

  async createSession(opts: { kind: SessionKind; workspacePath?: string; worktree?: boolean; agent?: string | null; projectId?: string | null }) {
    const model = await this.defaultModel();
    if (!model) throw new Error("no_provider_configured");
    const r = await this.client.session.create({
      kind: opts.kind,
      model,
      effort: this.ui.effort,
      incognito: opts.kind === "chat" ? this.ui.incognito : false,
      project_id: opts.projectId ?? null,
      workspace_path: opts.workspacePath ?? null,
      worktree: opts.kind === "code" ? (opts.worktree ? "new" : "auto") : null,
      agent: opts.agent ?? null,
    });
    const summary: SessionSummary = { session_id: r.session_id, kind: opts.kind, status: "idle", updated_at: new Date().toISOString(), project_id: opts.projectId ?? null, workspace_id: opts.workspacePath ?? null, title: null };
    if (!this.sessions.has(r.session_id)) {
      this.sessions = new Map(this.sessions).set(r.session_id, summary);
    }
    if (opts.workspacePath) {
      const recent = [opts.workspacePath, ...this.ui.recentWorkspaces.filter((w) => w !== opts.workspacePath)].slice(0, 8);
      this.setUi({ recentWorkspaces: recent });
    }
    this.selectSession(r.session_id);
    return r.session_id;
  }

  async deleteSession(id: string) {
    await this.client.session.delete({ session_id: id });
    this.closeSession(id);
    this.sessions = new Map(this.sessions);
    this.sessions.delete(id);
    if (this.ui.activeSession === id) this.setUi({ activeSession: null });
    this.changed();
  }

  // ---------- misc ----------

  onConfigChanged(f: Listener<string[]>) {
    this.configListeners.add(f);
    return () => this.configListeners.delete(f);
  }

  toast(t: Omit<Toast, "id">) {
    const toast = { ...t, id: ++this.toastId };
    this.toasts = [...this.toasts.slice(-3), toast];
    this.changedNow();
    setTimeout(() => this.dismissToast(toast.id), 8000);
  }

  dismissToast(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
    this.changedNow();
  }

  describeError(e: unknown): string {
    const kind = errorKind(e);
    if (kind === "not_driver") return "Another client is driving this session. Take over to continue.";
    if (kind === "method_not_found") return "This engine doesn't support that yet.";
    return errorMessage(e);
  }

  get selfId(): string {
    return this.info?.client_instance_id ?? "";
  }
}
