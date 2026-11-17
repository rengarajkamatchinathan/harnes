// Code mode pieces of the main column (05 §3.2): rewind dialog (D49), worktree finish
// footer (D50).
import { useEffect, useState } from "react";
import type { CheckpointPreview, RestoreWhat, WorktreeAction } from "@genie/protocol";
import { Badge, Button, Dialog, Segmented } from "@genie/ui";
import { t } from "../../i18n";
import type { MessageView } from "../../state/sessionModel";
import { useApp } from "../context";

export function RewindDialog({ sessionId, message, onClose, onDone }: { sessionId: string; message: MessageView; onClose: () => void; onDone: () => void }) {
  const app = useApp();
  const [restore, setRestore] = useState<RestoreWhat>("both");
  const [preview, setPreview] = useState<CheckpointPreview | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    app.client.checkpoint
      .preview({ session_id: sessionId, message_id: message.id, restore })
      .then(setPreview)
      .catch((e) => setErr(app.describeError(e)));
  }, [app, sessionId, message.id, restore]);
  const go = async () => {
    try {
      await app.client.checkpoint.rewind({ session_id: sessionId, message_id: message.id, restore });
      onDone();
      onClose();
    } catch (e) {
      setErr(app.describeError(e));
    }
  };
  return (
    <Dialog
      open
      title={t("code.rewindTitle")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={() => void go()}>
            {t("code.rewind")}
          </Button>
        </>
      }
    >
      <Segmented
        label={t("code.rewind")}
        value={restore}
        onChange={setRestore}
        options={[
          { value: "files", label: t("code.restoreFiles") },
          { value: "conversation", label: t("code.restoreConversation") },
          { value: "both", label: t("code.restoreBoth") },
        ]}
      />
      {preview && (
        <div className="stack" style={{ marginTop: 12 }}>
          {preview.files.map((f) => (
            <div key={f.path} className="row">
              <Badge tone={f.change === "added" ? "success" : f.change === "deleted" ? "danger" : "warning"}>{f.change[0]?.toUpperCase()}</Badge>
              <code>{f.path}</code>
            </div>
          ))}
          {preview.external_changes.length > 0 && <div className="muted">External changes: {preview.external_changes.join(", ")}</div>}
          {preview.skipped.length > 0 && <div className="muted">Skipped: {preview.skipped.join(", ")}</div>}
        </div>
      )}
      {err && <div className="error-text">{err}</div>}
    </Dialog>
  );
}

/** Merge into <base> · Create PR · Keep · Discard — only for `genie/*` worktree branches. */
export function WorktreeFooter({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const [branch, setBranch] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => {
    app.client.git
      .status({ session_id: sessionId })
      .then((r) => setBranch(r.branch ?? null))
      .catch(() => setBranch(null));
  }, [app, sessionId]);
  if (!branch?.startsWith("genie/")) return null;
  const finish = async (action: WorktreeAction) => {
    try {
      const r = await app.client.worktree.finish({ session_id: sessionId, action });
      setMsg(r.message ?? action);
    } catch (e) {
      setMsg(app.describeError(e));
    }
  };
  return (
    <div className="worktree-footer">
      <span className="muted">{t("code.isolated", { branch })}</span>
      <span className="grow" />
      <Button size="sm" onClick={() => void finish("merge")}>{t("code.finish.merge")}</Button>
      <Button size="sm" onClick={() => void finish("pr")}>{t("code.finish.pr")}</Button>
      <Button size="sm" onClick={() => void finish("keep")}>{t("code.finish.keep")}</Button>
      <Button size="sm" variant="ghost" onClick={() => void finish("discard")}>{t("code.finish.discard")}</Button>
      {msg && <span className="muted">{msg}</span>}
    </div>
  );
}
