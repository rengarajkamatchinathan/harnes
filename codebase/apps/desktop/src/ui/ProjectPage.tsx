// Project page (05 §2.4, D32): name, instructions, knowledge files, starred, chats, capacity.
import { useEffect, useRef, useState } from "react";
import type { IndexStatus } from "@genie/protocol";
import { Badge, Button, Input, Progress, Switch, Textarea } from "@genie/ui";
import { t } from "../i18n";
import { uploadBlob } from "../state/attachments";
import { useApp } from "./context";

export function ProjectPage({ id }: { id: string }) {
  const app = useApp();
  const p = app.projects.find((x) => x.project_id === id);
  const [name, setName] = useState(p?.name ?? "");
  const [instructions, setInstructions] = useState(p?.instructions ?? "");
  const [index, setIndex] = useState<IndexStatus | null>(null);
  const [files, setFiles] = useState<{ file_id: string; name: string }[]>([]);
  const fileRef = useRef<HTMLInputElement>(null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });

  useEffect(() => {
    setName(p?.name ?? "");
    setInstructions(p?.instructions ?? "");
  }, [p?.name, p?.instructions]);
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
        {files.map((f) => (
          <div key={f.file_id} className="row">
            <span className="grow">{f.name}</span>
            <Button size="sm" variant="ghost" onClick={() => void app.client.project.file.remove({ project_id: id, file_id: f.file_id }).then(() => setFiles((x) => x.filter((y) => y !== f)), fail)}>
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
                  const r = await app.client.project.file.add({ project_id: id, blob_id, name: f.name });
                  setFiles((x) => [...x, { file_id: r.file_id, name: f.name }]);
                } catch (err) {
                  fail(err);
                }
              }
            }}
          />
        </div>
        <h3 className="section-title">Chats</h3>
        {chats.map((s) => (
          <button key={s.session_id} className="list-button" onClick={() => app.selectSession(s.session_id)}>
            {s.title || t("sidebar.untitled")}
          </button>
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
