// Left sidebar (05 §2.1, §3.1, D54), laid out like Claude Desktop: title-bar strip (sidebar toggle, back/forward, Chat/Code
// switch), nav rows (New, Search, Projects, Customize), foldable Pinned/Chats sections (Chat) or
// sessions grouped by repo (Code), row actions, ⌘K search palette, profile footer.
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type React from "react";
import type { SessionSummary } from "@genie/protocol";
import { Badge, Button, Dialog, Icon, IconButton, Input, Menu, Select, Switch, Tooltip, type MenuItem } from "@genie/ui";
import { t } from "../i18n";
import type { App } from "../state/app";
import { useApp } from "./context";
import { NewCodeSession } from "./code/NewCodeSession";
import { SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, useShell } from "./shell";

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
              <Icon name={m === "chat" ? "chat" : "code"} />
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
  const drag = useRef<{ x: number; w: number } | null>(null);
  const clamp = (w: number) => Math.round(Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, w)));
  const shellEl = () => document.querySelector<HTMLElement>(".shell");
  return (
    <div
      className="sidebar__resize"
      role="separator"
      aria-orientation="vertical"
      aria-label={t("sidebar.resize")}
      aria-valuemin={SIDEBAR_MIN}
      aria-valuemax={SIDEBAR_MAX}
      aria-valuenow={app.ui.sidebarWidth}
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft") app.setUi({ sidebarWidth: clamp(app.ui.sidebarWidth - 16) });
        if (e.key === "ArrowRight") app.setUi({ sidebarWidth: clamp(app.ui.sidebarWidth + 16) });
      }}
      onDoubleClick={() => app.setUi({ sidebarWidth: SIDEBAR_DEFAULT })}
      onPointerDown={(e) => {
        drag.current = { x: e.clientX, w: app.ui.sidebarWidth };
        e.currentTarget.setPointerCapture(e.pointerId);
        shellEl()?.classList.add("shell--resizing");
      }}
      onPointerMove={(e) => {
        if (!drag.current) return;
        // Live-update the CSS variable; commit to state (and state.json) on release.
        shellEl()?.style.setProperty("--sidebar-w", `${clamp(drag.current.w + e.clientX - drag.current.x)}px`);
      }}
      onPointerUp={(e) => {
        if (!drag.current) return;
        const w = clamp(drag.current.w + e.clientX - drag.current.x);
        drag.current = null;
        shellEl()?.classList.remove("shell--resizing");
        app.setUi({ sidebarWidth: w });
      }}
    />
  );
}

export function Sidebar({ hidden = false }: { hidden?: boolean }) {
  const app = useApp();
  const { ui } = app;
  const shell = useShell();
  const [search, setSearch] = useState(false);
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

  const items = useMemo(() => [...app.sessions.values()].filter((s) => s.kind === ui.mode && !s.archived), [app.sessions, ui.mode]);
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
        <NavRow icon="search" label={t("sidebar.search")} onClick={() => setSearch(true)} />
        {ui.mode === "chat" && <NavRow icon="projects" label={t("sidebar.projects")} onClick={() => (app.projects.length ? app.setUi({ foldedSections: ui.foldedSections.filter((x) => x !== "projects") }) : setNewProject(true))} active={ui.view === "project"} />}
        <NavRow icon="customize" label={t("sidebar.customize")} onClick={() => app.openDeepLink(ui.mode === "code" ? "genie://settings/permissions" : "genie://settings/capabilities")} />
        {app.pendingAsks > 0 && (
          <div className="sidebar__asks">
            <Badge tone="warning">
              <Icon name="warning" /> {t("sidebar.pendingAsks", { n: app.pendingAsks })}
            </Badge>
          </div>
        )}
      </div>

      <div className="sidebar__list">
        {ui.mode === "chat" ? <ChatList items={items} app={app} onNewProject={() => setNewProject(true)} /> : <CodeList items={items} app={app} onNew={(ws) => setNewCode(ws)} />}
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
              <span className="sidebar__title">{name}</span>
              <Icon name="chevronDown" size={12} />
            </button>
          )}
        />
        <IconButton icon="settings" size="sm" label={t("sidebar.settings")} onClick={() => app.setUi({ view: "settings" })} />
      </div>

      {!shell.overlay && <SidebarResize />}
      {search && <SearchDialog onClose={() => setSearch(false)} />}
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

function CodeList({ items, app, onNew }: { items: SessionSummary[]; app: App; onNew: (workspace: string) => void }) {
  // Sessions grouped by repo (workspace) (05 §3.1).
  const groups = new Map<string, SessionSummary[]>();
  for (const s of [...items].sort((a, b) => b.updated_at.localeCompare(a.updated_at))) {
    const k = s.workspace_id ?? "—";
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k)!.push(s);
  }
  const fresh = useFreshIds(items.map((s) => s.session_id));
  if (!items.length) return null;
  return (
    <>
      {[...groups.entries()].map(([ws, list]) => {
        const folder = ws === "—" ? t("sidebar.noFolder") : (ws.split(/[\\/]/).filter(Boolean).pop() ?? ws);
        return (
          <Section key={ws} id={`ws:${ws}`} title={folder} action={<IconButton icon="plus" size="sm" label={t("sidebar.newInFolder", { folder })} onClick={() => onNew(ws === "—" ? "" : ws)} />}>
            {list.map((s) => (
              <SessionRow key={s.session_id} s={s} app={app} code fresh={fresh(s.session_id)} />
            ))}
          </Section>
        );
      })}
    </>
  );
}

/** ⌘K palette over titles and content (FTS, D24). */
function SearchDialog({ onClose }: { onClose: () => void }) {
  const app = useApp();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<string[] | null>(null);
  const [sel, setSel] = useState(0);
  useEffect(() => {
    if (!query.trim()) {
      setHits(null);
      return;
    }
    const h = setTimeout(() => {
      app.client.session
        .search({ query, scope: "all" })
        .then((r) => setHits([...new Set(r.hits.map((x) => x.session_id))]))
        .catch(() => setHits([...app.sessions.values()].filter((s) => (s.title ?? "").toLowerCase().includes(query.toLowerCase())).map((s) => s.session_id)));
    }, 150);
    return () => clearTimeout(h);
  }, [query, app]);
  const list = (hits ?? [...app.sessions.values()].filter((s) => !s.archived).sort((a, b) => b.updated_at.localeCompare(a.updated_at)).map((s) => s.session_id))
    .map((id) => app.sessions.get(id))
    .filter((s): s is SessionSummary => !!s)
    .slice(0, 30);
  const pick = (s: SessionSummary | undefined) => {
    if (!s) return;
    app.selectSession(s.session_id);
    onClose();
  };
  return (
    <div className="g-dialog__backdrop palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-modal="true" aria-label={t("search.title")}>
        <label className="palette__input">
          <Icon name="search" />
          <input
            autoFocus
            value={query}
            placeholder={t("search.placeholder")}
            aria-label={t("search.title")}
            onChange={(e) => (setQuery(e.target.value), setSel(0))}
            onKeyDown={(e) => {
              if (e.key === "Escape") onClose();
              else if (e.key === "ArrowDown") (e.preventDefault(), setSel((x) => Math.min(list.length - 1, x + 1)));
              else if (e.key === "ArrowUp") (e.preventDefault(), setSel((x) => Math.max(0, x - 1)));
              else if (e.key === "Enter") pick(list[sel]);
            }}
          />
        </label>
        <div className="palette__list" role="listbox">
          {!query && list.length > 0 && <div className="palette__group">{t("search.recent")}</div>}
          {list.map((s, i) => (
            <button key={s.session_id} role="option" aria-selected={i === sel} className={`palette__item ${i === sel ? "is-active" : ""}`} onMouseEnter={() => setSel(i)} onClick={() => pick(s)}>
              <Icon name={s.kind === "code" ? "code" : "chat"} />
              <span className="sidebar__title">{s.title || t("sidebar.untitled")}</span>
              <span className="muted palette__when">{new Date(s.updated_at).toLocaleDateString()}</span>
            </button>
          ))}
          {list.length === 0 && <div className="muted palette__empty">{query ? t("sidebar.noResults") : t("sidebar.empty")}</div>}
        </div>
      </div>
    </div>
  );
}

function SessionRow({ s, app, code, fresh }: { s: SessionSummary; app: App; code?: boolean; fresh?: boolean }) {
  const [renaming, setRenaming] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [moving, setMoving] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [title, setTitle] = useState(s.title ?? "");
  // Enter commits and unmounts the input, which then blurs: commit only once.
  const renameDone = useRef(false);
  const active = app.ui.view === "main" && app.ui.activeSession === s.session_id;
  const untitled = code ? t("sidebar.untitledSession") : t("sidebar.untitled");

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
  const items: MenuItem[] = [
    { id: "pin", label: s.pinned ? t("row.unpin") : t("row.pin"), icon: "pin", onSelect: () => void app.client.session.update({ session_id: s.session_id, pinned: !s.pinned }) },
    { id: "rename", label: t("row.rename"), icon: "edit", onSelect: startRename },
    ...(code ? [{ id: "archive", label: t("row.archive"), icon: "folder" as const, onSelect: () => void app.client.session.update({ session_id: s.session_id, archived: true }) }] : []),
    ...(!code ? [{ id: "move", label: t("row.moveToProject"), icon: "folder" as const, onSelect: () => setMoving(true) }] : []),
    { id: "md", label: t("row.exportMd"), icon: "download", separatorBefore: true, onSelect: () => void exportAs("markdown") },
    { id: "json", label: t("row.exportJson"), icon: "download", onSelect: () => void exportAs("json") },
    { id: "delete", label: t("row.delete"), icon: "trash", danger: true, separatorBefore: true, onSelect: () => setConfirm(true) },
  ];

  return (
    <div
      className={`sidebar__row ${active ? "is-active" : ""} ${fresh ? "is-new" : ""} ${renaming ? "is-renaming" : ""}`}
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
          onClick={() => app.selectSession(s.session_id)}
          onKeyDown={(e) => e.key === "F2" && (e.preventDefault(), startRename())}
          aria-current={active ? "page" : undefined}
        >
          {STATUS_TONE[s.status] ? <span className={`dot ${STATUS_TONE[s.status]}`} aria-label={s.status} /> : !code && <span className="bullet" aria-hidden />}
          <span className="sidebar__title">{s.title || untitled}</span>
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
      <Dialog open={moving} title={t("row.moveToProject")} onClose={() => setMoving(false)} footer={<Button onClick={() => setMoving(false)}>{t("common.close")}</Button>}>
        <Select
          value={s.project_id ?? ""}
          options={[{ value: "", label: t("row.noProject") }, ...app.projects.map((p) => ({ value: p.project_id, label: p.name }))]}
          aria-label={t("row.moveToProject")}
          onChange={(v) =>
            void app.moveToProject(s.session_id, v || null).then(
              () => (setMoving(false), app.toast({ title: v ? t("row.moved", { project: app.projects.find((p) => p.project_id === v)?.name ?? v }) : t("row.movedOut"), tone: "success" })),
              (e) => app.toast({ title: app.describeError(e), tone: "danger" }),
            )
          }
        />
      </Dialog>
    </div>
  );
}

function NewProject({ onClose }: { onClose: () => void }) {
  const app = useApp();
  const [name, setName] = useState("");
  const [instructions, setInstructions] = useState("");
  const [starred, setStarred] = useState(false);
  const create = async () => {
    try {
      const r = await app.client.project.create({ name, instructions });
      if (starred) await app.client.project.update({ project_id: r.project_id, starred: true });
      await app.refreshLists();
      app.setUi({ view: "project", activeProject: r.project_id });
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
