// Project page (05 §2.4, D32): name, instructions, knowledge files, starred, chats, capacity.
import { useEffect, useRef, useState } from "react";
import type { IndexStatus, ProjectFileInfo } from "@genie/protocol";
import { Badge, Icon, IconButton, Menu, Progress, Textarea } from "@genie/ui";
import { t } from "../i18n";
import { formatBytes, uploadBlob } from "../state/attachments";
import { useApp } from "./context";
import { NewProject, TitlebarControls } from "./Sidebar";
import { useShell } from "./shell";
import { GenieMark } from "./GenieMark";
import { ProjectBudgetCard } from "./BudgetGuard";

/** Projects overview: every project as a card, with search and "New project". */
export function ProjectsPage() {
  const shell = useShell();
  const app = useApp();
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState(false);
  const counts = new Map<string, { n: number; last: string | null }>();
  for (const s of app.sessions.values()) {
    if (!s.project_id || s.archived) continue;
    const c = counts.get(s.project_id) ?? { n: 0, last: null };
    c.n += 1;
    if (!c.last || s.updated_at > c.last) c.last = s.updated_at;
    counts.set(s.project_id, c);
  }
  const needle = q.trim().toLowerCase();
  const list = [...app.projects]
    .filter((p) => !needle || p.name.toLowerCase().includes(needle))
    .sort((a, b) => Number(!!b.starred) - Number(!!a.starred) || a.name.localeCompare(b.name));
  const chatsLabel = (n: number) => (n === 0 ? t("projects.noChats") : n === 1 ? t("projects.oneChat") : t("projects.chats", { n }));

  return (
    <div className="chat">
      <header className={`chat__header ${shell.sidebarHidden ? "chat__header--collapsed" : ""}`} data-tauri-drag-region>
        {shell.sidebarHidden && <TitlebarControls />}
      </header>
      <div className="page">
        <div className="page__inner projects">
          <div className="projects__head">
            <h1 className="projects__title">{t("sidebar.projects")}</h1>
            <button className="g-btn g-btn--primary projects__new" onClick={() => setCreating(true)}>
              <Icon name="plus" />
              <span>{t("sidebar.newProject")}</span>
            </button>
          </div>

          {app.projects.length > 0 && (
            <label className="projects__search">
              <Icon name="search" />
              <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("projects.search")} aria-label={t("projects.search")} />
            </label>
          )}

          {app.projects.length === 0 ? (
            <div className="projects__empty">
              <span className="project__icon" aria-hidden>
                <Icon name="projects" size={18} />
              </span>
              <strong>{t("projects.empty")}</strong>
              <p className="muted">{t("projects.emptyHint")}</p>
            </div>
          ) : list.length === 0 ? (
            <p className="muted projects__none">{t("sidebar.noResults")}</p>
          ) : (
            <div className="projects__grid">
              {list.map((p) => {
                const c = counts.get(p.project_id);
                return (
                  <button key={p.project_id} className="projects__card" onClick={() => app.setUi({ view: "project", activeProject: p.project_id })}>
                    <div className="projects__card-head">
                      <span className="project__icon" aria-hidden>
                        <Icon name="folder" size={16} />
                      </span>
                      <span className="projects__name">{p.name}</span>
                      {p.starred && <span className="projects__star" aria-label={t("project.unstar")}>★</span>}
                    </div>
                    {p.instructions?.trim() && <p className="projects__desc muted">{p.instructions}</p>}
                    <span className="projects__meta muted">
                      {chatsLabel(c?.n ?? 0)}
                      {c?.last && ` · ${new Date(c.last).toLocaleDateString()}`}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
      {creating && <NewProject onClose={() => setCreating(false)} />}
    </div>
  );
}

export function ProjectPage({ id }: { id: string }) {
  const shell = useShell();
  const app = useApp();
  const p = app.projects.find((x) => x.project_id === id);
  const [name, setName] = useState(p?.name ?? "");
  const [instructions, setInstructions] = useState(p?.instructions ?? "");
  const [index, setIndex] = useState<IndexStatus | null>(null);
  const [files, setFiles] = useState<ProjectFileInfo[] | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });

  useEffect(() => {
    setName(p?.name ?? "");
    setInstructions(p?.instructions ?? "");
  }, [p?.name, p?.instructions]);
  const loadFiles = () =>
    app.client.project.file
      .list({ project_id: id })
      .then((r) => setFiles(r.files))
      .catch(() => setFiles([]));
  useEffect(() => {
    void loadFiles();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app, id]);
  useEffect(() => {
    const load = () => app.client.project.index.status({ project_id: id }).then(setIndex).catch(() => setIndex(null));
    void load();
    const h = setInterval(load, 3000);
    return () => clearInterval(h);
  }, [app, id]);

  if (!p) return <div className="empty muted">{t("common.loading")}</div>;
  const save = () => void app.client.project.update({ project_id: id, name, instructions }).then(() => app.refreshLists(), fail);
  const chats = [...app.sessions.values()].filter((s) => s.project_id === id && !s.archived).sort((a, b) => b.updated_at.localeCompare(a.updated_at));

  return (
    <div className="chat">
      <header className={`chat__header ${shell.sidebarHidden ? "chat__header--collapsed" : ""}`} data-tauri-drag-region>
        {shell.sidebarHidden && <TitlebarControls />}
        <button className="chat__title chat__title--muted projects__crumb" onClick={() => app.setUi({ view: "project", activeProject: null })}>
          {t("sidebar.projects")}
        </button>
      </header>
      <div className="page">
        <div className="page__inner project">
          <div className="project__head">
            <span className="project__icon" aria-hidden>
              <Icon name="folder" size={18} />
            </span>
            <input className="project__name" value={name} onChange={(e) => setName(e.target.value)} onBlur={() => name.trim() && name !== p.name && save()} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} aria-label={t("project.name")} />
            <button
              className={`project__star ${p.starred ? "is-on" : ""}`}
              aria-pressed={!!p.starred}
              aria-label={p.starred ? t("project.unstar") : t("project.star")}
              title={p.starred ? t("project.unstar") : t("project.star")}
              onClick={() => void app.client.project.update({ project_id: id, starred: !p.starred }).then(() => app.refreshLists(), fail)}
            >
              {p.starred ? "★" : "☆"}
            </button>
          </div>

          <button className="project__new" onClick={() => app.setUi({ view: "main", activeSession: null, activeProject: id })}>
            <Icon name="plus" />
            <span>{t("project.newChat", { name: p.name })}</span>
          </button>

          <div className="project__grid">
            <section className="project__main">
              <h3 className="section-title">{t("project.chats")}</h3>
              {chats.length === 0 ? (
                <p className="muted settings-note">{t("project.noChats")}</p>
              ) : (
                <div className="list-box">
                  {chats.map((s) => (
                    <div key={s.session_id} className="list-box__row project__chat">
                      <button className="project__chat-link" onClick={() => app.selectSession(s.session_id)}>
                        <span className="sidebar__mark" aria-hidden>
                          <GenieMark size={14} />
                        </span>
                        <span className="sidebar__title">{s.title || t("sidebar.untitled")}</span>
                        <span className="muted project__when">{new Date(s.updated_at).toLocaleDateString()}</span>
                      </button>
                      <Menu
                        label={t("chat.options")}
                        align="right"
                        items={[{ id: "out", label: t("project.moveOut"), icon: "folder", onSelect: () => void app.moveToProject(s.session_id, null).catch(fail) }]}
                        trigger={(tp) => <IconButton icon="more" size="sm" label={t("chat.options")} {...tp} />}
                      />
                    </div>
                  ))}
                </div>
              )}
            </section>

            <aside className="project__side">
              <div className="project__card">
                <div className="project__card-head">
                  <strong>{t("project.instructions")}</strong>
                </div>
                <Textarea
                  className="project__instructions"
                  value={instructions}
                  placeholder={t("project.instructionsHint")}
                  onChange={(e) => setInstructions(e.target.value)}
                  onBlur={() => instructions !== (p.instructions ?? "") && save()}
                  aria-label={t("project.instructions")}
                />
              </div>

              <ProjectBudgetCard projectId={id} />

              <div className="project__card">
                <div className="project__card-head">
                  <strong>{t("project.knowledge")}</strong>
                  <IconButton icon="plus" size="sm" label={t("project.upload")} onClick={() => fileRef.current?.click()} />
                </div>
                {index && index.files > 0 && (
                  <div className="stack-tight">
                    <Progress value={index.capacity_pct / 100} warn={index.capacity_pct > 50} label={t("project.capacity")} />
                    <div className="row muted project__meta">
                      {t("project.capacityText", { pct: Math.round(index.capacity_pct) })}
                      {index.mode === "retrieval" && <Badge tone="info">{t("project.retrieval")}</Badge>}
                      {index.indexing && <span className="g-spinner" aria-label={t("project.indexing")} />}
                    </div>
                  </div>
                )}
                {files && !files.length && <p className="muted project__meta">{t("project.noFiles")}</p>}
                {(files ?? []).map((f) => (
                  <div key={f.file_id} className="row project__file">
                    <Icon name="file" size={14} />
                    <span className="grow sidebar__title">{f.name}</span>
                    <span className="muted project__meta">{[f.size != null ? formatBytes(f.size) : null, f.tokens != null ? `${f.tokens} tokens` : null].filter(Boolean).join(" · ")}</span>
                    <IconButton icon="trash" size="sm" label={t("common.delete")} onClick={() => void app.client.project.file.remove({ project_id: id, file_id: f.file_id }).then(loadFiles, fail)} />
                  </div>
                ))}
                <input
                  ref={fileRef}
                  type="file"
                  multiple
                  hidden
                  onChange={async (e) => {
                    for (const f of [...(e.target.files ?? [])]) {
                      try {
                        const blob_id = await uploadBlob(app.client, f);
                        await app.client.project.file.add({ project_id: id, blob_id, name: f.name });
                        await loadFiles();
                      } catch (err) {
                        fail(err);
                      }
                    }
                  }}
                />
              </div>
            </aside>
          </div>
        </div>
      </div>
    </div>
  );
}
