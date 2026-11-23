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
  type PreviewChanged,
  type ProjectInfo,
  type ProviderInfo,
  type SessionEventEnvelope,
  type SessionKind,
  type SessionListed,
  type SessionSummary,
  type SessionSynced,
  type TerminalExited,
  type TerminalOutput,
  type UserNotification,
  type WorkspaceSnapshot,
} from "@genie/protocol";
import { DEFAULT_APPEARANCE, type Appearance } from "@genie/ui";
import { errorKind, errorMessage, type HostInfo, type HostMessage, type HostStatus, type Platform } from "../platform";
import { Emitter } from "./emitter";
import { SessionStore } from "./session";
import { DevServers } from "./devServers";
import { Terminals } from "./terminals";

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
  /** Code mode's current project folder: the sidebar lists its sessions and new sessions open in it ("" = No project). */
  codeWorkspace: string;
  /** Open terminal ids per session, for reattach after a reload (05 §10). */
  terminals: Record<string, { id: string; title: string }[]>;
  /** OS notifications for paused asks when the window is in the background. */
  osNotifications: boolean;
  /** The D35 notice was shown before code execution was first turned on. */
  codeExecNoticeSeen: boolean;
  /** "What should Genie call you?" — greeting and sidebar footer. */
  displayName: string;
  sidebarCollapsed: boolean;
  /** Sidebar sections the user folded (e.g. "pinned", "recents", a workspace path). */
  foldedSections: string[];
  /** Chat list in the sidebar: split into Today / Yesterday / Previous 7 days / Older. */
  sidebarGroupByDate: boolean;
  /** Chat list filter: everything, or only chats that are working or waiting on you. */
  sidebarShow: "all" | "active";
  /** Slash commands used most recently first (the `/` menu's order, D58). */
  recentCommands: string[];
  /** Web links clicked in chat open in the Browser tab (Cmd/Ctrl-click: system browser). */
  browserOpenLinks: boolean;
  /** The first outside link from chat asked "Browser tab or default browser?" (t47). */
  browserLinkAsked: boolean;
  /** The right panel is minimized to a pill (t47); opening any tab restores it. */
  panelMinimized: boolean;
}

/** Folder paths compared loosely: the engine may report `\\?\C:\x` for a folder picked as `C:\x`. */
export function workspaceKey(p: string | null | undefined): string {
  return (p ?? "")
    .replace(/^\\\\\?\\/, "")
    .replace(/[\\/]+/g, "/")
    .replace(/\/$/, "")
    .toLowerCase();
}
export const sameWorkspace = (a: string | null | undefined, b: string | null | undefined) => workspaceKey(a) === workspaceKey(b);

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
  sidebarWidth: 280,
  incognito: false,
  appearance: DEFAULT_APPEARANCE,
  firstRunDone: false,
  model: null,
  effort: null,
  recentWorkspaces: [],
  codeWorkspace: "",
  terminals: {},
  osNotifications: true,
  codeExecNoticeSeen: false,
  displayName: "",
  sidebarCollapsed: false,
  foldedSections: [],
  sidebarGroupByDate: false,
  sidebarShow: "all",
  recentCommands: [],
  browserOpenLinks: true,
  browserLinkAsked: false,
  panelMinimized: false,
};

/** Where the user is, for the title-bar back/forward buttons. */
interface Loc {
  mode: SessionKind;
  view: View;
  activeSession: string | null;
  activeProject: string | null;
}
const locOf = (u: UiState): Loc => ({ mode: u.mode, view: u.view, activeSession: u.activeSession, activeProject: u.activeProject });
const sameLoc = (a: Loc, b: Loc) => a.mode === b.mode && a.view === b.view && a.activeSession === b.activeSession && a.activeProject === b.activeProject;

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
  /** Every catalog model, including providers that aren't connected (Settings shows these). */
  models: ModelInfo[] = [];
  providers: ProviderInfo[] = [];
  /** The `model` setting (default model for new sessions), if any. */
  configModel: string | null = null;
  toasts: Toast[] = [];

  private open = new Map<string, SessionStore>();
  /** server request id → ask id, for asks the daemon asked us directly. */
  private askReqs = new Map<string, number>();
  private reqAsks = new Map<number, string>();
  private everReady = false;
  private toastId = 0;
  private configListeners = new Set<Listener<string[]>>();
  /** Bumped on `commands.changed`; `/` menus reload `command.list` (06 §9.1). */
  commandsVersion = 0;
  /** Bumped by `/resume`: the sidebar opens its search dialog. */
  searchRequest = 0;
  private saveTimer: ReturnType<typeof setTimeout> | null = null;

  readonly terminals: Terminals;
  readonly devServers: DevServers;

  constructor(readonly platform: Platform) {
    super();
    this.client = new GenieClient({ request: (m, p) => platform.request(m, p) });
    this.terminals = new Terminals({ client: this.client, persistTerminals: (terminals) => this.setUi({ terminals }) });
    this.devServers = new DevServers(this.client);
  }

  /** Element select (Cmd/Ctrl+Shift+S, ticket 47): the Browser tab picks an element. */
  selectingElement = false;
  /** Text and files for a session's composer (Element select); `n` changes per inject. */
  composerInject: { sessionId: string; text: string; files: File[]; n: number } | null = null;

  setSelectingElement(on: boolean) {
    this.selectingElement = on;
    if (on) this.setUi({ panelOpen: true, panelTab: "browser" });
    this.changedNow();
  }

  injectComposer(sessionId: string, text: string, files: File[] = []) {
    this.composerInject = { sessionId, text, files, n: (this.composerInject?.n ?? 0) + 1 };
    this.changedNow();
  }

  /** The first outside link from chat, waiting for "Browser tab or default browser?". */
  linkAsk: { sid: string; href: string } | null = null;
  /** The user chose "Continue without saving" on the storage problem screen. */
  storageDismissed = false;

  askLinks(sid: string, href: string) {
    this.linkAsk = { sid, href };
    this.changedNow();
  }

  /** `true`: Browser tab, `false`: default browser (both remembered), `null`: dismissed. */
  answerLinks(pane: boolean | null) {
    const ask = this.linkAsk;
    this.linkAsk = null;
    if (!ask || pane === null) return this.changedNow();
    this.setUi({ browserOpenLinks: pane, browserLinkAsked: true });
    const external = () => void this.platform.openExternal(ask.href).catch(() => {});
    if (pane) {
      this.setUi({ panelOpen: true, panelTab: "browser" });
      void this.client.browser.tab.new({ session_id: ask.sid, url: ask.href }).catch(external);
    } else external();
  }

  /** A local file clicked in chat (HTML, PDF, video, SVG) in the session's Browser tab. */
  openFile(sessionId: string, path: string) {
    this.setUi({ panelOpen: true, panelTab: "browser" });
    void this.client.browser.open_file({ session_id: sessionId, path }).catch((e) => this.toast({ title: this.describeError(e), tone: "danger" }));
  }

  /** Show `url` in the session's Browser tab (a dev server, a server card's Open). */
  openInBrowser(sessionId: string, url: string, quiet = false) {
    this.setUi({ panelOpen: true, panelTab: "browser" });
    // Quiet for automatic opens: with no browser installed the tab already says so.
    void this.client.browser.navigate({ session_id: sessionId, url }).catch((e) => quiet || this.toast({ title: this.describeError(e), tone: "danger" }));
  }

  async start() {
    const [state] = await Promise.all([this.platform.loadState().catch(() => ({}))]);
    this.ui = { ...DEFAULT_UI, ...(state as Partial<UiState>), view: "main", appearance: { ...DEFAULT_APPEARANCE, ...((state as Partial<UiState>).appearance ?? {}) } };
    if ((state as { firstRunDone?: boolean }).firstRunDone) this.ui.firstRunDone = true;
    this.stateLoaded = true;
    this.terminals.restore(this.ui.terminals);
    this.info = await this.platform.connect((m) => this.onHost(m));
    this.changedNow();
  }

  // ---------- UI state ----------

  private backStack: Loc[] = [];
  private fwdStack: Loc[] = [];
  private navigating = false;

  get canGoBack() {
    return this.backStack.length > 0;
  }
  get canGoForward() {
    return this.fwdStack.length > 0;
  }
  goBack() {
    this.travel(this.backStack, this.fwdStack);
  }
  goForward() {
    this.travel(this.fwdStack, this.backStack);
  }
  private travel(from: Loc[], to: Loc[]) {
    const loc = from.pop();
    if (!loc) return;
    to.push(locOf(this.ui));
    this.navigating = true;
    try {
      this.setUi({ ...loc, activeSession: loc.activeSession && this.sessions.has(loc.activeSession) ? loc.activeSession : null });
    } finally {
      this.navigating = false;
    }
  }

  setUi(patch: Partial<UiState>) {
    // Showing a panel tab brings a minimized panel back.
    if (patch.panelOpen === true && patch.panelMinimized === undefined) patch = { ...patch, panelMinimized: false };
    const before = locOf(this.ui);
    this.ui = { ...this.ui, ...patch };
    const after = locOf(this.ui);
    // Settings is a modal over the current place, so it is not a history step.
    if (!this.navigating && !sameLoc(before, after) && before.view !== "settings" && after.view !== "settings") {
      this.backStack.push(before);
      if (this.backStack.length > 50) this.backStack.shift();
      this.fwdStack = [];
    }
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

  /** Open the session switcher with search (`/resume`). */
  openSearch() {
    this.searchRequest++;
    this.changed();
  }

  selectSession(id: string | null) {
    const s = id ? this.sessions.get(id) : null;
    this.setUi({
      activeSession: id,
      view: "main",
      ...(s && s.kind !== "unknown" ? { mode: s.kind } : {}),
      // Opening a Code session (from search, a link…) switches to its project.
      ...(s?.kind === "code" ? { codeWorkspace: s.workspace_id ?? "" } : {}),
    });
  }

  /** Project folders known to Code mode: recently used ones plus every folder that has sessions. */
  get codeWorkspaces(): string[] {
    const out: string[] = [];
    const add = (w: string | null | undefined) => {
      if (w && !out.some((x) => sameWorkspace(x, w))) out.push(w);
    };
    this.ui.recentWorkspaces.forEach(add);
    [...this.sessions.values()]
      .filter((s) => s.kind === "code" && !s.archived)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
      .forEach((s) => add(s.workspace_id));
    return out;
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

  /** Hide (or, with `false`, show again) the storage problem screen. */
  setStorageDismissed(v: boolean) {
    this.storageDismissed = v;
    this.changedNow();
  }

  /** `secrets.unlock` succeeded: the engine is usable now (D26). */
  markUnlocked() {
    this.status = { ...this.status, locked: undefined };
    this.changedNow();
    void this.onReady();
  }

  private async onReady() {
    if (this.everReady) {
      // The relay resubscribed sessions and the workspace feed itself (02 §3.1).
      void this.refreshLists();
      this.terminals.reattachAll();
      this.devServers.refreshAll();
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
    const [projects, models, providers, cfg] = await Promise.allSettled([this.client.project.list({}), this.client.model.list({}), this.client.provider.list({}), this.client.config.get({ keys: ["model"] })]);
    if (projects.status === "fulfilled") this.projects = projects.value.projects;
    if (models.status === "fulfilled") this.models = models.value.models;
    if (providers.status === "fulfilled") this.providers = providers.value.providers;
    if (cfg.status === "fulfilled") this.configModel = typeof cfg.value.values.model === "string" && cfg.value.values.model ? cfg.value.values.model : null;
    this.changed();
  }

  /**
   * Models a new turn can actually use: the catalog lists every provider's models, but only
   * a provider that is Ready (credential present, endpoint set, not blocked) can serve one.
   */
  get usableModels(): ModelInfo[] {
    const ready = new Set(this.providers.filter((p) => p.status === "ready").map((p) => p.id));
    return this.models.filter((m) => ready.has(m.provider));
  }

  /** Model for new sessions: last picked, else the configured default, else the first usable; never one whose provider isn't connected. */
  preferredModel(): string | null {
    const usable = this.usableModels;
    const ok = (id: string | null | undefined) => !!id && usable.some((m) => m.id === id);
    if (ok(this.ui.model)) return this.ui.model!;
    if (ok(this.configModel)) return this.configModel;
    return usable[0]?.id ?? null;
  }

  private onNotification(method: string, params: unknown) {
    switch (method) {
      case "event": {
        const env = params as SessionEventEnvelope;
        if (env.event.type === "terminal.output") {
          this.terminals.onOutput(env.event as unknown as TerminalOutput);
          return;
        }
        this.open.get(env.session_id)?.apply(env);
        return;
      }
      case "terminal.exited":
        this.terminals.onExited(params as TerminalExited);
        return;
      case "preview.changed": {
        // A dev server that comes up in the session on screen opens in its Browser tab.
        const p = params as PreviewChanged;
        const up = this.devServers.onChanged(p);
        const first = up.find((s) => s.url);
        if (first?.url && p.session_id === this.ui.activeSession) this.openInBrowser(p.session_id, first.url, true);
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
        if (p.change === "deleted") {
          this.sessions.delete(p.session.session_id);
          if (this.terminals.list(p.session.session_id).length) this.terminals.forgetSession(p.session.session_id);
        }
        else this.sessions.set(p.session.session_id, p.session);
        this.changed();
        return;
      }
      case "config.changed":
      case "policy.changed": {
        const keys = (params as { keys?: string[] }).keys ?? [];
        this.configListeners.forEach((f) => f(keys));
        if (!keys.length || keys.some((k) => k === "model" || k.startsWith("providers"))) void this.refreshLists();
        return;
      }
      case "commands.changed":
        this.commandsVersion++;
        this.changed();
        return;
      case "catalog.updated":
      case "provider.status":
        void this.refreshLists();
        return;
      case "notification": {
        const n = params as UserNotification;
        this.toast({ title: n.title, body: n.body, tone: "info", link: n.link });
        this.osNotify(n.title, n.body ?? undefined);
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
    // The turn is paused on us: tell the user if Genie isn't in front (05 §4 notifications).
    const title = this.sessions.get(ask.session_id)?.title;
    this.osNotify("Genie needs your approval", [ask.tool ?? ask.kind, title].filter(Boolean).join(" · "));
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

  /** Model for new sessions (see `preferredModel`). */
  async defaultModel(): Promise<string | null> {
    return this.preferredModel();
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
    if (opts.kind === "code") this.setUi({ codeWorkspace: opts.workspacePath ?? "" });
    this.selectSession(r.session_id);
    return r.session_id;
  }

  /** Move a Chat into a project, or out of it with `null` (`session.update.project_id`, `""` = out). */
  async moveToProject(id: string, projectId: string | null) {
    await this.client.session.update({ session_id: id, project_id: projectId ?? "" });
    const s = this.sessions.get(id);
    if (s) {
      this.sessions = new Map(this.sessions).set(id, { ...s, project_id: projectId });
      this.changed();
    }
  }

  /** After "Delete all data": forget everything local and reload from a fresh snapshot. */
  async reloadAfterDeleteAll() {
    for (const id of [...this.open.keys()]) this.closeSession(id);
    for (const id of [...this.sessions.keys()]) this.terminals.forgetSession(id);
    this.sessions = new Map();
    this.setUi({ activeSession: null, activeProject: null, view: "main", terminals: {} });
    try {
      await this.client.workspace.subscribe({});
    } catch (e) {
      this.platform.log("warn", `workspace.subscribe: ${errorMessage(e)}`);
    }
    await this.refreshLists();
  }

  async deleteSession(id: string) {
    await this.client.session.delete({ session_id: id });
    this.closeSession(id);
    this.terminals.forgetSession(id);
    this.sessions = new Map(this.sessions);
    this.sessions.delete(id);
    if (this.ui.activeSession === id) this.setUi({ activeSession: null });
    this.changed();
  }

  // ---------- misc ----------

  /** Is the app window in front? (OS notifications only go out when it isn't.) */
  isForeground: () => boolean = () => typeof document !== "undefined" && !document.hidden && document.hasFocus();

  /** OS notification when the window is in the background and the user allows them. */
  osNotify(title: string, body?: string) {
    if (!this.ui.osNotifications || this.isForeground()) return;
    void this.platform.notify(title, body).catch((e) => this.platform.log("warn", `notification: ${errorMessage(e)}`));
  }

  onConfigChanged(f: Listener<string[]>) {
    this.configListeners.add(f);
    return () => {
      this.configListeners.delete(f);
    };
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

  /** Name for the greeting and profile row: the one set in Settings, else the OS account's. */
  get userName(): string {
    return this.ui.displayName.trim() || this.info?.user_name?.trim() || "";
  }
}
