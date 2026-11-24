// Left sidebar (05 §2.1, §3.1, D54), laid out like Claude Desktop: title-bar strip (sidebar toggle, back/forward, Chat/Code
// switch), nav rows (New, Search, Projects, Customize), foldable Pinned/Chats sections (Chat) or
// sessions grouped by repo (Code), row actions, ⌘K search palette, profile footer.
import { useEffect, useId, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type React from "react";
import type { SessionSummary } from "@genie/protocol";
import { Badge, Button, Dialog, Icon, IconButton, Input, Menu, Switch, Tooltip, type MenuItem } from "@genie/ui";
import { t } from "../i18n";
import { sameWorkspace, type App } from "../state/app";
import { useApp } from "./context";
import { NewCodeSession } from "./code/NewCodeSession";
import { CommandPalette, modKey } from "./CommandPalette";
import { SIDEBAR_DEFAULT, SIDEBAR_MIN, useShell } from "./shell";
import { ResizeHandle } from "./ResizeHandle";
import { ShareHandoff } from "./Handoff";

export type DateGroup = "pinned" | "today" | "yesterday" | "week" | "older";

/** Recents grouped by date (t31); pinned first. Pure for tests. */
export function groupByDate(items: SessionSummary[], now: Date = new Date()): [DateGroup, SessionSummary[]][] {
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const day = 86_400_000;
  const groups: Record<DateGroup, SessionSummary[]> = { pinned: [], today: [], yesterday: [], week: [], older: [] };
  const sorted = [...items].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  for (const s of sorted) {
    if (s.pinned) groups.pinned.push(s);
    else {
      const ts = Date.parse(s.updated_at);
      if (ts >= start) groups.today.push(s);
      else if (ts >= start - day) groups.yesterday.push(s);
      else if (ts >= start - 7 * day) groups.week.push(s);
      else groups.older.push(s);
    }
  }
  return (Object.entries(groups) as [DateGroup, SessionSummary[]][]).filter(([, v]) => v.length);
}

/** Compact "last activity" label for a chat row: now, 5m, 3h, 2d, then "Nov 3". Pure for tests. */
export function shortAgo(iso: string, now: number = Date.now()): string {
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return "";
  const mins = Math.floor((now - ts) / 60_000);
  if (mins < 1) return t("sidebar.justNow");
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

// One shared minute tick so every row's relative time stays current without a timer per row.
let minuteNow = Date.now();
const minuteSubs = new Set<() => void>();
let minuteTimer: ReturnType<typeof setInterval> | undefined;
function subscribeMinute(cb: () => void) {
  minuteSubs.add(cb);
  minuteTimer ??= setInterval(() => {
    minuteNow = Date.now();
    minuteSubs.forEach((f) => f());
  }, 60_000);
  return () => {
    minuteSubs.delete(cb);
    if (!minuteSubs.size && minuteTimer) (clearInterval(minuteTimer), (minuteTimer = undefined));
  };
}
const useMinute = () => useSyncExternalStore(subscribeMinute, () => minuteNow);

const STATUS_TONE: Record<string, string> = { running: "dot--running", awaiting_ask: "dot--ask", paused: "dot--ask", interrupted: "dot--interrupted", compacting: "dot--running", interrupting: "dot--running" };

/** Working or waiting on the user: what "Active only" keeps in the chat list. */
const ACTIVE = new Set(["running", "compacting", "interrupting", "awaiting_ask", "paused"]);

const DATE_LABEL: Record<DateGroup, Parameters<typeof t>[0]> = { pinned: "sidebar.pinned", today: "sidebar.today", yesterday: "sidebar.yesterday", week: "sidebar.week", older: "sidebar.older" };

/** Sidebar toggle, back/forward and the Chat/Code switch, in the window's title-bar strip. */
export function TitlebarControls() {
  const app = useApp();
  const { ui } = app;
  const shell = useShell();
  // Inline sidebar: collapse/expand (persisted). Overlay layout: open/close the drawer.
  const toggle = () => (shell.overlay ? shell.setDrawer(!shell.drawerOpen) : app.setUi({ sidebarCollapsed: !ui.sidebarCollapsed }));
  return (
    <div className="titlebar__controls">
      <IconButton icon="sidebar" size="sm" label={t("sidebar.toggle")} shortcut={isMac() ? "⌘." : "Ctrl+."} onClick={toggle} />
      <IconButton icon="chevronLeft" size="sm" label={t("sidebar.back")} disabled={!app.canGoBack} onClick={() => app.goBack()} />
      <IconButton icon="chevronRight" size="sm" label={t("sidebar.forward")} disabled={!app.canGoForward} onClick={() => app.goForward()} />
      {/* Segmented Chat/Code switch; the thumb slides between the two. */}
      <div className="modeswitch" role="group" aria-label="Mode" data-active={ui.mode}>
        <span className="modeswitch__thumb" aria-hidden />
        {(["chat", "code"] as const).map((m) => (
          <Tooltip key={m} content={t(`mode.${m}`)}>
            <button className="modeswitch__btn" aria-pressed={ui.mode === m} aria-label={t(`mode.${m}`)} onClick={() => ui.mode !== m && app.setUi({ mode: m, activeSession: null, view: "main", activeProject: null })}>
              <Icon name={m === "chat" ? "modeChat" : "modeCode"} />
            </button>
          </Tooltip>
        ))}
      </div>
    </div>
  );
}

function NavRow({ icon, label, onClick, active, badge }: { icon: Parameters<typeof Icon>[0]["name"]; label: string; onClick: () => void; active?: boolean; badge?: React.ReactNode }) {
  return (
    <button className={`nav-row ${active ? "is-active" : ""}`} onClick={onClick}>
      <Icon name={icon} />
      <span className="sidebar__title">{label}</span>
      {badge}
    </button>
  );
}

/**
 * Foldable sidebar section. The body stays mounted and animates its height (0fr ↔ 1fr) so
 * folding slides closed rather than snapping. `persistentAction` keeps the heading buttons
 * visible; otherwise they appear on hover.
 */
function Section({ id, title, children, action, persistentAction }: { id: string; title: string; children: React.ReactNode; action?: React.ReactNode; persistentAction?: boolean }) {
  const app = useApp();
  const bodyId = useId();
  const folded = app.ui.foldedSections.includes(id);
  const toggle = () => app.setUi({ foldedSections: folded ? app.ui.foldedSections.filter((x) => x !== id) : [...app.ui.foldedSections, id] });
  return (
    <section className={`sidebar__section ${folded ? "is-folded" : ""}`}>
      <div className="sidebar__heading">
        <button className="sidebar__fold" aria-expanded={!folded} aria-controls={bodyId} onClick={toggle}>
          <span className="sidebar__heading-text">{title}</span>
          <Icon name="chevronDown" size={12} />
        </button>
        {action && <div className={`sidebar__heading-actions ${persistentAction ? "is-persistent" : ""}`}>{action}</div>}
      </div>
      <div className="sidebar__section-body" id={bodyId} inert={folded} aria-hidden={folded || undefined}>
        <div className="sidebar__section-inner">{children}</div>
      </div>
    </section>
  );
}

/**
 * Ids that appeared since the previous render (a new chat, or rows a filter change reveals),
 * so they can ease in. Nothing animates on first mount.
 */
function useFreshIds(ids: string[]): (id: string) => boolean {
  const seen = useRef<Set<string> | null>(null);
  const until = useRef(new Map<string, number>());
  const now = Date.now();
  if (seen.current) for (const id of ids) if (!seen.current.has(id)) until.current.set(id, now + 600);
  seen.current = new Set(ids);
  return (id) => (until.current.get(id) ?? 0) > now;
}

export function initials(name: string) {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "G";
  return (parts[0][0] + (parts.length > 1 ? parts[parts.length - 1][0] : "")).toUpperCase();
}

const isMac = () => typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);

/** Drag the sidebar's right edge to resize it; double-click resets; arrow keys step by 16 px. */
function SidebarResize() {
  const app = useApp();
  const shell = useShell();
  return (
    <ResizeHandle
      edge="right"
      className="sidebar__resize"
      cssVar="--sidebar-w"
      label={t("sidebar.resize")}
      value={app.ui.sidebarWidth}
      min={SIDEBAR_MIN}
      max={shell.sidebarMax}
      defaultValue={SIDEBAR_DEFAULT}
      onCommit={(w) => app.setUi({ sidebarWidth: w })}
    />
  );
}

export function Sidebar({ hidden = false }: { hidden?: boolean }) {
  const app = useApp();
  const { ui } = app;
  const shell = useShell();
  const [search, setSearch] = useState(false);
  // `/resume` from a composer (06 §9.1).
  const searchRequest = app.searchRequest;
  useEffect(() => {
    if (searchRequest) setSearch(true);
  }, [searchRequest]);
  const [newCode, setNewCode] = useState<string | null>(null);
  const [newProject, setNewProject] = useState(false);

  // ⌘K / Ctrl+K opens search, ⌘N / Ctrl+N starts a new chat or session, ⌘, opens settings.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey) return;
      if (e.key === "k") {
        e.preventDefault();
        setSearch(true);
      } else if (e.key === ",") {
        e.preventDefault();
        app.setUi({ view: "settings" });
      } else if (e.key === ".") {
        e.preventDefault();
        if (shell.overlay) shell.setDrawer(!shell.drawerOpen);
        else app.setUi({ sidebarCollapsed: !app.ui.sidebarCollapsed });
      } else if (e.key === "n") {
        e.preventDefault();
        app.setUi({ activeSession: null, view: "main", activeProject: null });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [app, shell]);

  // Side chats belong to their Code session's panel, not the chat list.
  const sideChatIds = useMemo(() => new Set(Object.values(ui.sideChats)), [ui.sideChats]);
  const items = useMemo(() => [...app.sessions.values()].filter((s) => s.kind === ui.mode && !s.archived && !sideChatIds.has(s.session_id)), [app.sessions, ui.mode, sideChatIds]);
  const isNew = ui.view === "main" && !ui.activeSession;
  const startNew = () => app.setUi({ activeSession: null, view: "main", activeProject: null });
  const name = app.userName || t("sidebar.you");
  const account = app.info?.user_name?.trim();
  const go = (section: string) => () => app.openDeepLink(`genie://settings/${section}`);

  return (
    <nav className="sidebar" aria-label="Sidebar" inert={hidden} aria-hidden={hidden || undefined}>
      <div className="titlebar" data-tauri-drag-region>
        <TitlebarControls />
      </div>

      <div className="sidebar__nav">
        <NavRow icon="plus" label={t("sidebar.new")} onClick={startNew} active={isNew} />
        <NavRow icon="search" label={t("sidebar.search")} onClick={() => setSearch(true)} badge={<kbd className="nav-row__kbd">{modKey()}K</kbd>} />
        {ui.mode === "chat" && <NavRow icon="projects" label={t("sidebar.projects")} onClick={() => app.setUi({ view: "project", activeProject: null, activeSession: null })} active={ui.view === "project" && !ui.activeProject} />}
        <NavRow icon="customize" label={t("sidebar.customize")} onClick={() => app.openDeepLink("genie://settings/general")} />
        <Menu
          label={t("sidebar.more")}
          className="nav-more__list"
          items={[
            { id: "settings", label: t("sidebar.settings"), icon: "settings", shortcut: isMac() ? "⌘," : "Ctrl+,", onSelect: () => app.setUi({ view: "settings" }) },
            { id: "usage", label: t("settings.section.usage"), icon: "chart", onSelect: go("usage") },
            { id: "connectors", label: t("settings.section.connectors"), icon: "plug", onSelect: go("connectors") },
            { id: "handoff", label: t("handoff.open"), icon: "download", separatorBefore: true, onSelect: () => app.pickHandoffFile?.() },
            { id: "developer", label: t("settings.section.developer"), icon: "code", onSelect: go("developer") },
          ]}
          trigger={(p) => (
            <button className="nav-row" {...p}>
              <Icon name="more" />
              <span className="sidebar__title">{t("sidebar.more")}</span>
            </button>
          )}
        />
        {app.pendingAsks > 0 && (
          <div className="sidebar__asks">
            <Badge tone="warning">
              <Icon name="warning" /> {t("sidebar.pendingAsks", { n: app.pendingAsks })}
            </Badge>
          </div>
        )}
      </div>

      <div className="sidebar__list">
        {ui.mode === "chat" ? <ChatList items={items} app={app} onNewProject={() => setNewProject(true)} /> : <CodeList items={items} app={app} />}
      </div>

      <div className="sidebar__footer">
        <Menu
          label={t("sidebar.account")}
          placement="up"
          className="profile-menu"
          header={account ? t("sidebar.signedIn", { name: account }) : t("sidebar.localProfile")}
          items={[
            { id: "settings", label: t("sidebar.settings"), icon: "settings", shortcut: isMac() ? "⌘," : "Ctrl+,", onSelect: () => app.setUi({ view: "settings" }) },
            { id: "account", label: t("settings.section.account"), icon: "user", onSelect: go("account") },
            { id: "usage", label: t("settings.section.usage"), icon: "chart", onSelect: go("usage") },
            { id: "appearance", label: t("settings.section.appearance"), icon: "palette", onSelect: go("appearance") },
            { id: "connectors", label: t("settings.section.connectors"), icon: "plug", onSelect: go("connectors") },
            { id: "developer", label: t("settings.section.developer"), icon: "code", separatorBefore: true, onSelect: go("developer") },
          ]}
          trigger={(p) => (
            <button className="profile" {...p}>
              <span className="avatar" aria-hidden>
                {initials(app.userName)}
              </span>
              <span className="profile__name">
                <span className="sidebar__title">{name}</span>
                {account && account !== name && <span className="profile__org"> · {account}</span>}
              </span>
              <Icon name="chevronDown" size={14} />
            </button>
          )}
        />
      </div>

      {!shell.overlay && <SidebarResize />}
      {search && <CommandPalette onClose={() => setSearch(false)} onNewProject={() => setNewProject(true)} onNewCode={() => setNewCode("")} />}
      {newCode !== null && <NewCodeSession initialPath={newCode || undefined} onClose={() => setNewCode(null)} />}
      {newProject && <NewProject onClose={() => setNewProject(false)} />}
    </nav>
  );
}

function ChatList({ items, app, onNewProject }: { items: SessionSummary[]; app: App; onNewProject: () => void }) {
  const { ui } = app;
  const sorted = [...items].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const pinned = sorted.filter((s) => s.pinned);
  const unpinned = sorted.filter((s) => !s.pinned);
  const rest = ui.sidebarShow === "active" ? unpinned.filter((s) => ACTIVE.has(s.status)) : unpinned;
  const fresh = useFreshIds(sorted.map((s) => s.session_id));
  const filtered = ui.sidebarShow !== "all" || ui.sidebarGroupByDate;
  const row = (s: SessionSummary) => <SessionRow key={s.session_id} s={s} app={app} fresh={fresh(s.session_id)} />;
  const filter = (
    <Menu
      label={t("sidebar.filter")}
      align="right"
      items={[
        { id: "flat", group: t("sidebar.view"), label: t("sidebar.listFlat"), checked: !ui.sidebarGroupByDate, onSelect: () => app.setUi({ sidebarGroupByDate: false }) },
        { id: "date", group: t("sidebar.view"), label: t("sidebar.listByDate"), checked: ui.sidebarGroupByDate, onSelect: () => app.setUi({ sidebarGroupByDate: true }) },
        { id: "all", group: t("sidebar.show"), label: t("sidebar.showAll"), checked: ui.sidebarShow === "all", onSelect: () => app.setUi({ sidebarShow: "all" }) },
        { id: "active", group: t("sidebar.show"), label: t("sidebar.showActive"), checked: ui.sidebarShow === "active", onSelect: () => app.setUi({ sidebarShow: "active" }) },
      ]}
      trigger={(p) => <IconButton icon="filter" size="sm" label={t("sidebar.filter")} className={filtered ? "is-on" : undefined} {...p} />}
    />
  );
  return (
    <>
      {app.projects.length > 0 && (
        <Section id="projects" title={t("sidebar.projects")} action={<IconButton icon="plus" size="sm" label={t("sidebar.newProject")} onClick={onNewProject} />}>
          {[...app.projects]
            .sort((a, b) => Number(!!b.starred) - Number(!!a.starred))
            .map((p) => (
              <div key={p.project_id} className={`sidebar__row ${ui.view === "project" && ui.activeProject === p.project_id ? "is-active" : ""}`}>
                <button className="sidebar__link" onClick={() => app.setUi({ view: "project", activeProject: p.project_id })}>
                  <Icon name="folder" />
                  <span className="sidebar__title">{p.name}</span>
                  {p.starred && <span className="muted" aria-label="Starred">★</span>}
                </button>
              </div>
            ))}
        </Section>
      )}
      {pinned.length > 0 && (
        <Section id="pinned" title={t("sidebar.pinned")}>
          {pinned.map(row)}
        </Section>
      )}
      {unpinned.length > 0 && (
        <Section id="recents" title={t("sidebar.chatsAndTasks")} action={filter} persistentAction>
          {rest.length === 0 && <div className="sidebar__empty muted">{t("sidebar.noActive")}</div>}
          {ui.sidebarGroupByDate
            ? groupByDate(rest).map(([g, list]) => (
                <div key={g} className="sidebar__dategroup">
                  <div className="sidebar__group">{t(DATE_LABEL[g])}</div>
                  {list.map(row)}
                </div>
              ))
            : rest.map(row)}
        </Section>
      )}
    </>
  );
}

export const folderName = (p: string) => p.replace(/^\\\\\?\\/, "").split(/[\\/]/).filter(Boolean).pop() ?? p;

/**
 * Code mode's project switcher (D54): pick a project folder, or "No project". The sidebar then
 * lists only that project's sessions, and New starts sessions in it.
 */
export function CodeProjectPicker({ app, variant = "sidebar" }: { app: App; variant?: "sidebar" | "inline" | "card" }) {
  const current = app.ui.codeWorkspace;
  const known = app.codeWorkspaces;
  const choose = (w: string) => app.setUi({ codeWorkspace: w, activeSession: null, view: "main" });
  const open = () =>
    void app.platform.pickFolder(t("code.folder")).then(
      (p) => {
        if (!p) return;
        app.setUi({ recentWorkspaces: [p, ...app.ui.recentWorkspaces.filter((w) => !sameWorkspace(w, p))].slice(0, 8) });
        choose(p);
      },
      () => {},
    );
  const items: MenuItem[] = [
    { id: "none", label: t("code.noProject"), icon: "chat", checked: !current, onSelect: () => choose("") },
    ...known.map((w, i) => ({ id: `ws:${w}`, label: folderName(w), title: w, icon: "repo" as const, checked: !!current && sameWorkspace(current, w), separatorBefore: i === 0, onSelect: () => choose(w) })),
    { id: "open", label: t("code.openFolder"), icon: "plus", separatorBefore: true, onSelect: open },
  ];
  return (
    <Menu
      label={t("code.project")}
      className="codeproj__list"
      items={items}
      trigger={(p) => (
        <button className={`codeproj codeproj--${variant}`} title={current || t("code.noProject")} {...p}>
          {variant === "sidebar" && <Icon name={current ? "repo" : "chat"} size={16} />}
          {variant === "inline" && <Icon name="folder" size={14} />}
          <span className="codeproj__name">{current ? folderName(current) : variant === "card" ? t("code.chooseProject") : t("code.noProject")}</span>
          <Icon name="chevronDown" size={12} />
        </button>
      )}
    />
  );
}

function CodeList({ items, app }: { items: SessionSummary[]; app: App }) {
  // Projects: one row per folder; clicking a project opens its page (its sessions, New session).
  // All sessions: every Code session, whatever its project.
  const sorted = [...items].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const projects = app.codeWorkspaces;
  const fresh = useFreshIds(sorted.map((s) => s.session_id));
  const newIn = (w: string) => app.setUi({ codeWorkspace: w, activeSession: null, view: "main", activeProject: null });
  const openPage = (w: string) => app.setUi({ view: "folder", codeWorkspace: w, activeSession: null, activeProject: null });
  const onPage = (w: string) => app.ui.view === "folder" && sameWorkspace(app.ui.codeWorkspace, w);
  const openFolder = () =>
    void app.platform.pickFolder(t("code.folder")).then(
      (p) => {
        if (!p) return;
        app.setUi({ recentWorkspaces: [p, ...app.ui.recentWorkspaces.filter((w) => !sameWorkspace(w, p))].slice(0, 8) });
        openPage(p);
      },
      () => {},
    );
  return (
    <>
      {projects.length > 0 && (
      <Section id="code:projects" title={t("code.projects")} action={<IconButton icon="plus" size="sm" label={t("code.openFolder")} onClick={openFolder} />} persistentAction>
        {projects.map((w) => {
          const name = folderName(w);
          const count = sorted.filter((s) => sameWorkspace(s.workspace_id, w)).length;
          const active = onPage(w);
          return (
            <div key={w} className={`sidebar__row ${active ? "is-active" : ""}`}>
              <button className="sidebar__link" title={w} aria-current={active ? "page" : undefined} onClick={() => openPage(w)}>
                <Icon name="repo" />
                <span className="sidebar__title">{name}</span>
                <span className="codeproj-row__count muted">{count}</span>
              </button>
              <IconButton icon="plus" size="sm" label={t("sidebar.newInFolder", { folder: name })} className="sidebar__more" onClick={() => newIn(w)} />
            </div>
          );
        })}
      </Section>
      )}
      {sorted.length === 0 ? (
        <div className="sidebar__blank">
          <span className="muted">{t("code.sessionsEmpty")}</span>
          <PixelSparkles />
        </div>
      ) : (
        <Section id="code:all" title={t("code.allSessions")}>
          {sorted.map((s) => <SessionRow key={s.session_id} s={s} app={app} code fresh={fresh(s.session_id)} tip={s.workspace_id ? folderName(s.workspace_id) : t("code.noProject")} />)}
        </Section>
      )}
    </>
  );
}

/** Faint pixel-art sparkles under the empty session list (decorative). */
const SPARKLE_PIXELS: [number, number, number][] = [
  // [x, y, shade 1–3] on a 10 × 10 grid: one large four-point star and two small ones.
  [5, 0, 2], [4, 1, 1], [5, 1, 3], [6, 1, 1], [3, 2, 1], [4, 2, 2], [5, 2, 3], [6, 2, 2], [7, 2, 1], [4, 3, 1], [5, 3, 3], [6, 3, 1], [5, 4, 2],
  [1, 4, 1], [0, 5, 1], [1, 5, 2], [2, 5, 1], [1, 6, 1],
  [8, 5, 1], [7, 6, 1], [8, 6, 2], [9, 6, 1], [8, 7, 1], [8, 8, 1],
  [2, 8, 1], [5, 7, 1], [5, 9, 1],
];
function PixelSparkles() {
  return (
    <svg className="pixel-sparkles" width={80} height={80} viewBox="0 0 10 10" aria-hidden shapeRendering="crispEdges">
      {SPARKLE_PIXELS.map(([x, y, shade]) => (
        <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill="currentColor" opacity={shade === 3 ? 0.5 : shade === 2 ? 0.32 : 0.16} />
      ))}
    </svg>
  );
}

function SessionRow({ s, app, code, fresh, hint, tip }: { s: SessionSummary; app: App; code?: boolean; fresh?: boolean; hint?: string; tip?: string }) {
  const [renaming, setRenaming] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [moving, setMoving] = useState(false);
  const [sharing, setSharing] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [title, setTitle] = useState(s.title ?? "");
  // Enter commits and unmounts the input, which then blurs: commit only once.
  const renameDone = useRef(false);
  const active = app.ui.view === "main" && app.ui.activeSession === s.session_id;
  const untitled = code ? t("sidebar.untitledSession") : t("sidebar.untitled");
  const ago = shortAgo(s.updated_at, useMinute());

  const startRename = () => {
    setTitle(s.title ?? "");
    renameDone.current = false;
    setRenaming(true);
  };
  const finishRename = (save: boolean) => {
    if (renameDone.current) return;
    renameDone.current = true;
    setRenaming(false);
    const next = title.trim();
    if (save && next && next !== (s.title ?? "")) void app.client.session.update({ session_id: s.session_id, title: next }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  };

  const exportAs = async (format: "markdown" | "json") => {
    try {
      const r = await app.client.session.export({ session_id: s.session_id, format });
      const name = `${(s.title ?? "chat").slice(0, 60)}.${format === "markdown" ? "md" : "json"}`;
      const path = await app.platform.saveFile(name, { text: r.text ?? "" });
      app.toast({ title: t("row.exported", { path }), tone: "success" });
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const items: MenuItem[] = [
    { id: "pin", label: s.pinned ? t("row.unpin") : t("row.pin"), icon: "pin", onSelect: () => void app.client.session.update({ session_id: s.session_id, pinned: !s.pinned }).catch(fail) },
    { id: "rename", label: t("row.rename"), icon: "edit", keepFocus: true, onSelect: startRename },
    ...(code ? [{ id: "archive", label: t("row.archive"), icon: "folder" as const, onSelect: () => void app.client.session.update({ session_id: s.session_id, archived: true }).catch(fail) }] : []),
    ...(!code ? [{ id: "move", label: t("row.moveToProject"), icon: "folder" as const, keepFocus: true, onSelect: () => setMoving(true) }] : []),
    { id: "md", label: t("row.exportMd"), icon: "download", separatorBefore: true, onSelect: () => void exportAs("markdown") },
    { id: "json", label: t("row.exportJson"), icon: "download", onSelect: () => void exportAs("json") },
    { id: "handoff", label: t("handoff.share"), icon: "share", keepFocus: true, onSelect: () => setSharing(true) },
    { id: "delete", label: t("row.delete"), icon: "trash", danger: true, separatorBefore: true, keepFocus: true, onSelect: () => setConfirm(true) },
  ];

  return (
    <div
      className={`sidebar__row ${code ? "sidebar__row--code" : ""} ${active ? "is-active" : ""} ${fresh ? "is-new" : ""} ${renaming ? "is-renaming" : ""} ${hint ? "" : "has-time"}`}
      onContextMenu={(e) => {
        // Right-click opens the same actions menu as the ··· button.
        if (renaming) return;
        e.preventDefault();
        setMenuOpen(true);
      }}
    >
      {renaming ? (
        <input
          className="sidebar__rename"
          autoFocus
          value={title}
          aria-label={t("row.rename")}
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={() => finishRename(true)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              finishRename(true);
            } else if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              finishRename(false);
            }
          }}
        />
      ) : (
        <button
          className="sidebar__link"
          title={tip ? `${s.title || untitled} · ${tip}` : undefined}
          onClick={() => app.selectSession(s.session_id)}
          onKeyDown={(e) => e.key === "F2" && (e.preventDefault(), startRename())}
          aria-current={active ? "page" : undefined}
        >
          {/* Only sessions that need attention get a mark; idle titles sit flush with the headings. */}
          {STATUS_TONE[s.status] && (
            <span className="sidebar__mark">
              <span className={`dot ${STATUS_TONE[s.status]}`} aria-label={s.status} />
            </span>
          )}
          <span className="sidebar__title">{s.title || untitled}</span>
          {hint ? (
            <span className="sidebar__hint muted">{hint}</span>
          ) : (
            <time className="sidebar__time" dateTime={s.updated_at} title={new Date(s.updated_at).toLocaleString()}>
              {ago}
            </time>
          )}
        </button>
      )}
      <Menu label={t("row.actions")} align="right" items={items} open={menuOpen} onOpenChange={setMenuOpen} trigger={(p) => <IconButton icon="more" size="sm" label={t("row.actions")} className="sidebar__more" {...p} />} />
      <Dialog
        open={confirm}
        title={t("row.delete")}
        onClose={() => setConfirm(false)}
        footer={
          <>
            <Button onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
            <Button variant="danger" onClick={() => (setConfirm(false), void app.deleteSession(s.session_id).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" })))}>
              {t("common.delete")}
            </Button>
          </>
        }
      >
        {t("row.deleteConfirm", { title: s.title || untitled })}
      </Dialog>
      {moving && <MoveToProject s={s} onClose={() => setMoving(false)} />}
      {sharing && <ShareHandoff s={s} onClose={() => setSharing(false)} />}
    </div>
  );
}

/** "Move to project": one click on a project moves the chat; or create a project and move into it. */
function MoveToProject({ s, onClose }: { s: SessionSummary; onClose: () => void }) {
  const app = useApp();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const current = s.project_id ?? null;
  const move = async (projectId: string | null, label?: string) => {
    if (busy || projectId === current) return onClose();
    setBusy(true);
    try {
      await app.moveToProject(s.session_id, projectId);
      app.toast({ title: projectId ? t("row.moved", { project: label ?? app.projects.find((p) => p.project_id === projectId)?.name ?? projectId }) : t("row.movedOut"), tone: "success" });
      onClose();
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
      setBusy(false);
    }
  };
  const createAndMove = async () => {
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    try {
      const r = await app.client.project.create({ name: n, instructions: "" });
      await app.refreshLists();
      setBusy(false);
      await move(r.project_id, n);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
      setBusy(false);
    }
  };
  const projects = [...app.projects].sort((a, b) => Number(!!b.starred) - Number(!!a.starred) || a.name.localeCompare(b.name));
  return (
    <Dialog open title={t("row.moveToProject")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.cancel")}</Button>}>
      <div className="moveto">
        <p className="muted moveto__sub">{t("row.moveHint", { title: s.title || t("sidebar.untitled") })}</p>
        <div className="moveto__list" role="listbox" aria-label={t("row.moveToProject")}>
          {projects.length === 0 && <p className="muted moveto__empty">{t("row.noProjectsYet")}</p>}
          {projects.map((p) => (
            <button key={p.project_id} role="option" aria-selected={p.project_id === current} className={`moveto__item ${p.project_id === current ? "is-current" : ""}`} disabled={busy} onClick={() => void move(p.project_id, p.name)}>
              <Icon name="folder" />
              <span className="moveto__name">{p.name}</span>
              {p.starred && <span className="muted" aria-hidden>★</span>}
              {p.project_id === current && <Icon name="check" />}
            </button>
          ))}
          {current && (
            <button className="moveto__item moveto__item--out" disabled={busy} onClick={() => void move(null)}>
              <Icon name="x" />
              <span className="moveto__name">{t("project.moveOut")}</span>
            </button>
          )}
        </div>
        <form
          className="moveto__new"
          onSubmit={(e) => {
            e.preventDefault();
            void createAndMove();
          }}
        >
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("row.newProjectName")} aria-label={t("row.newProjectName")} disabled={busy} />
          <Button type="submit" variant="primary" disabled={!name.trim() || busy}>
            {t("row.createAndMove")}
          </Button>
        </form>
      </div>
    </Dialog>
  );
}

/** `stay`: select the new project for the chat being started instead of opening its page. */
export function NewProject({ onClose, stay }: { onClose: () => void; stay?: boolean }) {
  const app = useApp();
  const [name, setName] = useState("");
  const [instructions, setInstructions] = useState("");
  const [starred, setStarred] = useState(false);
  const create = async () => {
    try {
      const r = await app.client.project.create({ name, instructions });
      if (starred) await app.client.project.update({ project_id: r.project_id, starred: true });
      await app.refreshLists();
      app.setUi(stay ? { activeProject: r.project_id } : { view: "project", activeProject: r.project_id });
      onClose();
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };
  return (
    <Dialog
      open
      title={t("sidebar.newProject")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={!name.trim()} onClick={() => void create()}>
            {t("common.save")}
          </Button>
        </>
      }
    >
      <div className="stack">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Name" aria-label="Name" autoFocus />
        <textarea className="g-textarea" value={instructions} onChange={(e) => setInstructions(e.target.value)} placeholder="Instructions" aria-label="Instructions" />
        <Switch checked={starred} onChange={setStarred} label="Starred" />
      </div>
    </Dialog>
  );
}
