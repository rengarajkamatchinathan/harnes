// Project page (05 §2.4, D32): name, instructions, knowledge files, starred, chats, capacity.
import { useEffect, useRef, useState } from "react";
import type { IndexStatus, ProjectFileInfo } from "@genie/protocol";
import { Badge, Icon, IconButton, Menu, Progress, Textarea } from "@genie/ui";
import { t } from "../i18n";
import { formatBytes, uploadBlob } from "../state/attachments";
import { useApp } from "./context";
import { TitlebarControls } from "./Sidebar";
import { useShell } from "./shell";

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
        <span className="chat__title chat__title--muted">{t("sidebar.projects")}</span>
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
