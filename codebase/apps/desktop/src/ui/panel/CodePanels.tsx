// Code right-panel tabs (05 §3.3): Changes (git status/diff, commit split button,
// checkpoints), Files (tree with M/A/D markers, read-only preview), Terminal (stub until
// the daemon serves terminal.*).
import { useEffect, useMemo, useState } from "react";
import type { CheckpointEntry, CommitThen, FileChange, FileNode } from "@genie/protocol";
import { Badge, Button, Icon, Input, Menu, Segmented } from "@genie/ui";
import { t } from "../../i18n";
import { highlight } from "../../markdown/markdown";
import { useApp, useAsync, useSession } from "../context";

const MARK: Record<string, string> = { added: "A", modified: "M", deleted: "D" };

/** Split a unified diff into per-file chunks. */
export function splitDiff(diff: string): { path: string; text: string }[] {
  const out: { path: string; text: string }[] = [];
  for (const chunk of diff.split(/^(?=diff --git )/m)) {
    if (!chunk.trim()) continue;
    const m = /^diff --git a\/(\S+) b\/(\S+)/.exec(chunk);
    out.push({ path: m ? m[2] : "(diff)", text: chunk });
  }
  return out;
}

/** Side-by-side rows from a unified diff hunk (split view). */
export function sideBySide(text: string): [string, string][] {
  const rows: [string, string][] = [];
  let dels: string[] = [];
  let adds: string[] = [];
  const flush = () => {
    for (let i = 0; i < Math.max(dels.length, adds.length); i++) rows.push([dels[i] ?? "", adds[i] ?? ""]);
    dels = [];
    adds = [];
  };
  for (const line of text.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ")) continue;
    if (line.startsWith("-")) dels.push(line.slice(1));
    else if (line.startsWith("+")) adds.push(line.slice(1));
    else {
      flush();
      rows.push([line.slice(1), line.slice(1)]);
    }
  }
  flush();
  return rows;
}

export function ChangesPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const store = useSession(sessionId);
  const rev = store?.model.status === "idle" ? store.model.rev : -1; // refresh when turns settle
  const status = useAsync(() => app.client.git.status({ session_id: sessionId }), [sessionId, rev === -1 ? 0 : rev]);
  const diff = useAsync(() => app.client.git.diff({ session_id: sessionId }), [sessionId, rev === -1 ? 0 : rev]);
  const cps = useAsync(() => app.client.checkpoint.list({ session_id: sessionId }), [sessionId, rev === -1 ? 0 : rev]);
  const [mode, setMode] = useState<"unified" | "split">("unified");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const files = useMemo(() => splitDiff(diff.data?.text ?? ""), [diff.data]);

  const commit = async (then: CommitThen | null) => {
    setBusy(true);
    try {
      const r = await app.client.git.commit({ session_id: sessionId, message, then });
      app.toast({ title: `Committed ${r.commit.slice(0, 8)}`, body: r.note ?? r.pr_url ?? undefined, tone: "success", link: null });
      setMessage("");
      status.reload();
      diff.reload();
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };

  if (status.error) return <div className="empty muted">{app.describeError(status.error)}</div>;
  const changed: FileChange[] = status.data?.files ?? [];
  return (
    <div className="stack panel-pad">
      <div className="row">
        <Icon name="branch" /> <strong>{status.data?.branch ?? "—"}</strong>
        <span className="grow" />
        <Segmented label="Diff view" value={mode} onChange={setMode} options={[{ value: "unified", label: t("code.unified") }, { value: "split", label: t("code.split") }]} />
      </div>
      <div className="row">
        <Input value={message} onChange={(e) => setMessage(e.target.value)} placeholder={t("code.commitMessage")} aria-label={t("code.commitMessage")} />
        <div className="g-btn-group">
          <Button variant="primary" disabled={busy || !message.trim() || !changed.length} onClick={() => void commit(null)}>
            {t("code.commit")}
          </Button>
          <Menu
            label={t("code.commit")}
            align="right"
            trigger={(p) => <Button variant="primary" icon="chevronDown" aria-label="More commit options" disabled={busy || !message.trim() || !changed.length} {...p} />}
            items={[
              { id: "push", label: t("code.commitPush"), onSelect: () => void commit("push") },
              { id: "pr", label: t("code.createPr"), onSelect: () => void commit("pr") },
            ]}
          />
        </div>
      </div>
      {!changed.length && <div className="muted">{t("code.noChanges")}</div>}
      {changed.map((f) => {
        const d = files.find((x) => x.path === f.path);
        return (
          <details key={f.path} className="file-diff">
            <summary>
              <Badge tone={f.change === "added" ? "success" : f.change === "deleted" ? "danger" : "warning"}>{MARK[f.change] ?? "?"}</Badge> <code>{f.path}</code>
            </summary>
            {d &&
              (mode === "unified" ? (
                <pre className="code diff" dangerouslySetInnerHTML={{ __html: highlight(d.text, "diff") }} />
              ) : (
                <table className="split-diff">
                  <tbody>
                    {sideBySide(d.text).map(([a, b], i) => (
                      <tr key={i}>
                        <td className={a !== b ? "del" : ""}>{a}</td>
                        <td className={a !== b ? "add" : ""}>{b}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ))}
          </details>
        );
      })}
      <h3 className="section-title">{t("code.checkpoints")}</h3>
      {(cps.data?.checkpoints ?? []).map((c: CheckpointEntry) => (
        <div key={c.message_id} className="row">
          <code className="grow">{c.message_id.slice(0, 12)}</code>
          <span className="muted">{c.files_changed} files</span>
        </div>
      ))}
      {cps.error ? <div className="muted">{app.describeError(cps.error)}</div> : null}
    </div>
  );
}

export function FilesPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const tree = useAsync(() => app.client.files.tree({ session_id: sessionId }), [sessionId]);
  const status = useAsync(() => app.client.git.status({ session_id: sessionId }), [sessionId]);
  const [open, setOpen] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const file = useAsync(() => (open ? app.client.files.read({ session_id: sessionId, path: open }) : Promise.resolve(null)), [sessionId, open]);
  const marks = new Map((status.data?.files ?? []).map((f) => [f.path, MARK[f.change] ?? ""]));
  const nodes: FileNode[] = (tree.data?.nodes ?? []).filter((n) => !filter || n.path.toLowerCase().includes(filter.toLowerCase()));
  if (tree.error) return <div className="empty muted">{app.describeError(tree.error)}</div>;
  if (open)
    return (
      <div className="stack panel-pad">
        <div className="row">
          <Button size="sm" icon="chevronLeft" onClick={() => setOpen(null)}>
            {t("settings.back")}
          </Button>
          <code className="grow">{open}</code>
        </div>
        {file.data?.text != null ? <pre className="code" dangerouslySetInnerHTML={{ __html: highlight(file.data.text, open.split(".").pop()) }} /> : <div className="muted">{file.loading ? t("common.loading") : `${file.data?.size ?? 0} bytes (binary)`}</div>}
      </div>
    );
  return (
    <div className="stack panel-pad">
      <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={t("sidebar.search")} aria-label={t("sidebar.search")} />
      {!nodes.length && <div className="muted">{tree.loading ? t("common.loading") : t("code.emptyFiles")}</div>}
      <ul className="tree">
        {nodes.slice(0, 2000).map((n) => (
          <li key={n.path} style={{ paddingLeft: (n.path.split("/").length - 1) * 12 }}>
            <button className="list-button" disabled={n.dir} onClick={() => setOpen(n.path)}>
              <Icon name={n.dir ? "folder" : "file"} /> {n.path.split("/").pop()}
              {marks.get(n.path) && <Badge tone="warning">{marks.get(n.path)}</Badge>}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function TerminalPanel() {
  useEffect(() => {}, []);
  return (
    <div className="empty">
      <Icon name="terminal" />
      <strong>{t("panel.comingSoon")}</strong>
      <p className="muted">{t("panel.terminalSoon")}</p>
    </div>
  );
}
