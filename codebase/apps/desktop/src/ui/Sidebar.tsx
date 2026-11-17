// Left sidebar (05 §2.1, §3.1): mode switch, New, Search, Projects, Recents by date (Chat)
// or sessions grouped by repo (Code); row actions; footer with Settings.
import { useEffect, useMemo, useState } from "react";
import type { SessionSummary } from "@genie/protocol";
import { Badge, Button, Dialog, Icon, IconButton, Input, Menu, Segmented, Select, Switch, type MenuItem } from "@genie/ui";
import { t } from "../i18n";
import type { App } from "../state/app";
import { useApp } from "./context";
import { NewCodeSession } from "./code/NewCodeSession";

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

const GROUP_LABEL: Record<DateGroup, Parameters<typeof t>[0]> = {
  pinned: "sidebar.pinned",
  today: "sidebar.today",
  yesterday: "sidebar.yesterday",
  week: "sidebar.week",
  older: "sidebar.older",
};

const STATUS_TONE: Record<string, string> = { running: "dot--running", awaiting_ask: "dot--ask", paused: "dot--ask", interrupted: "dot--interrupted", compacting: "dot--running", interrupting: "dot--running" };

export function Sidebar() {
  const app = useApp();
  const { ui } = app;
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Set<string> | null>(null);
  const [newCode, setNewCode] = useState(false);
  const [newProject, setNewProject] = useState(false);

  // FTS over titles and content (D24), debounced.
  useEffect(() => {
    if (!query.trim()) {
      setHits(null);
      return;
    }
    const h = setTimeout(() => {
      app.client.session
        .search({ query, scope: "all" })
        .then((r) => setHits(new Set(r.hits.map((x) => x.session_id))))
        .catch(() => setHits(new Set([...app.sessions.values()].filter((s) => (s.title ?? "").toLowerCase().includes(query.toLowerCase())).map((s) => s.session_id))));
    }, 200);
    return () => clearTimeout(h);
  }, [query, app]);

  const items = useMemo(() => [...app.sessions.values()].filter((s) => s.kind === ui.mode && !s.archived && (!hits || hits.has(s.session_id))), [app.sessions, ui.mode, hits]);

  const startNew = () => {
    if (ui.mode === "code") setNewCode(true);
    else app.setUi({ activeSession: null, view: "main", activeProject: null });
  };

  return (
    <nav className="sidebar" aria-label="Sidebar">
      <div className="sidebar__top">
        <div className="brand">
          <Icon name="sparkle" /> {t("app.name")}
        </div>
        <Segmented
          label="Mode"
          value={ui.mode}
          onChange={(mode) => app.setUi({ mode, activeSession: null, view: "main" })}
          options={[
            { value: "chat", label: t("mode.chat") },
            { value: "code", label: t("mode.code") },
          ]}
        />
        <div className="row">
          <Button variant="primary" icon="plus" onClick={startNew} style={{ flex: 1 }}>
            {ui.mode === "code" ? t("sidebar.newSession") : t("sidebar.newChat")}
          </Button>
          {ui.mode === "chat" && (
            <IconButton
              icon="incognito"
              label={t("sidebar.incognito")}
              variant={ui.incognito ? "primary" : "secondary"}
              aria-pressed={ui.incognito}
              onClick={() => app.setUi({ incognito: !ui.incognito, activeSession: null, view: "main" })}
            />
          )}
        </div>
        <label className="search">
          <Icon name="search" />
          <span className="g-sr-only">{t("sidebar.search")}</span>
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("sidebar.searchPlaceholder")} />
        </label>
        {app.pendingAsks > 0 && (
          <Badge tone="warning">
            <Icon name="warning" /> {t("sidebar.pendingAsks", { n: app.pendingAsks })}
          </Badge>
        )}
      </div>

      <div className="sidebar__list">
        {ui.mode === "chat" && !query && (
          <section>
            <h3 className="sidebar__heading">
              {t("sidebar.projects")}
              <IconButton icon="plus" size="sm" label={t("sidebar.newProject")} onClick={() => setNewProject(true)} />
            </h3>
            {[...app.projects]
              .sort((a, b) => Number(!!b.starred) - Number(!!a.starred))
              .map((p) => (
                <button key={p.project_id} className={`sidebar__row ${ui.view === "project" && ui.activeProject === p.project_id ? "is-active" : ""}`} onClick={() => app.setUi({ view: "project", activeProject: p.project_id })}>
                  <Icon name="folder" />
                  <span className="sidebar__title">{p.name}</span>
                  {p.starred && <span aria-label="Starred">★</span>}
                </button>
              ))}
          </section>
        )}

        {ui.mode === "chat" ? (
          <section>
            {!query && <h3 className="sidebar__heading">{t("sidebar.recents")}</h3>}
            {items.length === 0 && <div className="muted sidebar__empty">{query ? t("sidebar.noResults") : t("sidebar.empty")}</div>}
            {groupByDate(items).map(([g, list]) => (
              <div key={g}>
                <div className="sidebar__group">{t(GROUP_LABEL[g])}</div>
                {list.map((s) => (
                  <SessionRow key={s.session_id} s={s} app={app} />
                ))}
              </div>
            ))}
          </section>
        ) : (
          <CodeList items={items} app={app} />
        )}
      </div>

      <div className="sidebar__footer">
        <Button variant="ghost" icon="settings" onClick={() => app.setUi({ view: "settings" })}>
          {t("sidebar.settings")}
        </Button>
      </div>

      {newCode && <NewCodeSession onClose={() => setNewCode(false)} />}
      {newProject && <NewProject onClose={() => setNewProject(false)} />}
    </nav>
  );
}

function CodeList({ items, app }: { items: SessionSummary[]; app: App }) {
  // Sessions grouped by repo (workspace) (05 §3.1).
  const groups = new Map<string, SessionSummary[]>();
  for (const s of [...items].sort((a, b) => b.updated_at.localeCompare(a.updated_at))) {
    const k = s.workspace_id ?? "—";
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k)!.push(s);
  }
  if (!items.length) return <div className="muted sidebar__empty">{t("sidebar.empty")}</div>;
  return (
    <section>
      {[...groups.entries()].map(([ws, list]) => (
        <div key={ws}>
          <div className="sidebar__group" title={ws}>
            <Icon name="folder" /> {ws.split(/[\\/]/).filter(Boolean).pop() ?? ws}
          </div>
          {list.map((s) => (
            <SessionRow key={s.session_id} s={s} app={app} code />
          ))}
        </div>
      ))}
    </section>
  );
}

function SessionRow({ s, app, code }: { s: SessionSummary; app: App; code?: boolean }) {
  const [renaming, setRenaming] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [moving, setMoving] = useState(false);
  const [title, setTitle] = useState(s.title ?? "");
  const active = app.ui.view === "main" && app.ui.activeSession === s.session_id;

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
    { id: "rename", label: t("row.rename"), icon: "edit", onSelect: () => setRenaming(true) },
    { id: "pin", label: s.pinned ? t("row.unpin") : t("row.pin"), icon: "pin", onSelect: () => void app.client.session.update({ session_id: s.session_id, pinned: !s.pinned }) },
    ...(code ? [{ id: "archive", label: t("row.archive"), icon: "folder" as const, onSelect: () => void app.client.session.update({ session_id: s.session_id, archived: true }) }] : []),
    ...(!code ? [{ id: "move", label: t("row.moveToProject"), icon: "folder" as const, onSelect: () => setMoving(true) }] : []),
    { id: "md", label: t("row.exportMd"), icon: "download", onSelect: () => void exportAs("markdown") },
    { id: "json", label: t("row.exportJson"), icon: "download", onSelect: () => void exportAs("json") },
    { id: "delete", label: t("row.delete"), icon: "trash", danger: true, onSelect: () => setConfirm(true) },
  ];

  return (
    <div className={`sidebar__row ${active ? "is-active" : ""}`}>
      {renaming ? (
        <form
          className="row grow"
          onSubmit={(e) => {
            e.preventDefault();
            setRenaming(false);
            void app.client.session.update({ session_id: s.session_id, title });
          }}
        >
          <Input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} onBlur={() => setRenaming(false)} aria-label={t("row.rename")} />
        </form>
      ) : (
        <button className="sidebar__link" onClick={() => app.selectSession(s.session_id)} aria-current={active ? "page" : undefined}>
          {code && <span className={`dot ${STATUS_TONE[s.status] ?? ""}`} aria-label={s.status} />}
          <span className="sidebar__title">{s.title || t("sidebar.untitled")}</span>
          {!code && STATUS_TONE[s.status] && <span className={`dot ${STATUS_TONE[s.status]}`} aria-label={s.status} />}
        </button>
      )}
      <Menu label={t("row.rename")} align="right" items={items} trigger={(p) => <IconButton icon="more" size="sm" label="Actions" className="sidebar__more" {...p} />} />
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
        {t("row.deleteConfirm", { title: s.title || t("sidebar.untitled") })}
      </Dialog>
      <Dialog open={moving} title={t("row.moveToProject")} onClose={() => setMoving(false)} footer={<Button onClick={() => setMoving(false)}>{t("common.close")}</Button>}>
        <p className="muted">{t("settings.notServed")}</p>
        <Select disabled options={app.projects.map((p) => ({ value: p.project_id, label: p.name }))} aria-label={t("row.moveToProject")} />
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
