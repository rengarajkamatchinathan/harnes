// Project page (05 §2.4, D32): name, instructions, knowledge files, starred, chats, capacity.
import { useEffect, useRef, useState } from "react";
import type { IndexStatus, ProjectFileInfo } from "@genie/protocol";
import { Badge, Button, Input, Progress, Switch, Textarea } from "@genie/ui";
import { t } from "../i18n";
import { formatBytes, uploadBlob } from "../state/attachments";
import { useApp } from "./context";

export function ProjectPage({ id }: { id: string }) {
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
  const chats = [...app.sessions.values()].filter((s) => s.project_id === id);

  return (
    <div className="page">
      <div className="page__inner stack">
        <div className="row">
          <Input value={name} onChange={(e) => setName(e.target.value)} onBlur={save} aria-label="Name" className="title-input" />
          <Switch checked={!!p.starred} onChange={(v) => void app.client.project.update({ project_id: id, starred: v }).then(() => app.refreshLists(), fail)} label="Starred" />
        </div>
        <label className="stack-tight">
          <span>Instructions</span>
          <Textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} onBlur={save} />
        </label>
        <h3 className="section-title">Knowledge</h3>
        {index && (
          <div className="stack-tight">
            <Progress value={index.capacity_pct / 100} warn={index.capacity_pct > 50} label="Capacity" />
            <div className="row muted">
              {index.files} files · {index.tokens} tokens · {Math.round(index.capacity_pct)}% of the window
              {index.mode === "retrieval" && <Badge tone="info">Using retrieval</Badge>}
              {index.indexing && <span className="g-spinner" aria-label="Indexing" />}
            </div>
          </div>
        )}
        {files && !files.length && <p className="muted">{t("project.noFiles")}</p>}
        {(files ?? []).map((f) => (
          <div key={f.file_id} className="row">
            <span className="grow">{f.name}</span>
            <span className="muted">
              {[f.size != null ? formatBytes(f.size) : null, f.tokens != null ? `${f.tokens} tokens` : null].filter(Boolean).join(" · ")}
            </span>
            <Button size="sm" variant="ghost" onClick={() => void app.client.project.file.remove({ project_id: id, file_id: f.file_id }).then(loadFiles, fail)}>
              {t("common.delete")}
            </Button>
          </div>
        ))}
        <div>
          <Button icon="paperclip" onClick={() => fileRef.current?.click()}>
            Upload files
          </Button>
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
        <h3 className="section-title">Chats</h3>
        {chats.map((s) => (
          <div key={s.session_id} className="row">
            <button className="list-button grow" onClick={() => app.selectSession(s.session_id)}>
              {s.title || t("sidebar.untitled")}
            </button>
            <Button size="sm" variant="ghost" onClick={() => void app.moveToProject(s.session_id, null).catch(fail)}>
              {t("project.moveOut")}
            </Button>
          </div>
        ))}
        <div>
          <Button variant="primary" icon="plus" onClick={() => app.setUi({ view: "main", activeSession: null, activeProject: id })}>
            {t("sidebar.newChat")}
          </Button>
        </div>
      </div>
    </div>
  );
}
