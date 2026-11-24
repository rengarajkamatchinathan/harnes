// Code mode pieces of the main column (05 §3.2): rewind dialog (D49), worktree finish
// footer (D50).
import { useEffect, useState } from "react";
import type { CheckpointPreview, RestoreWhat, WorktreeAction } from "@genie/protocol";
import { Badge, Button, Dialog, Icon, Menu, Segmented } from "@genie/ui";
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

/**
 * "Working in an isolated copy · branch genie/x" and a Finish ⌄ menu (Merge into base · Create
 * PR · Keep · Discard), only for `genie/*` worktree branches. Results show as toasts (the PR
 * commands in a dialog to copy); Discard asks first; the bar goes once the copy is removed.
 */
export function WorktreeFooter({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const [branch, setBranch] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [pr, setPr] = useState<string | null>(null);
  useEffect(() => {
    app.client.git
      .status({ session_id: sessionId })
      .then((r) => setBranch(r.branch ?? null))
      .catch(() => setBranch(null));
  }, [app, sessionId]);
  if (!branch?.startsWith("genie/")) return null;
  const finish = async (action: WorktreeAction) => {
    setBusy(true);
    try {
      const r = await app.client.worktree.finish({ session_id: sessionId, action });
      if (action === "pr") setPr(r.message ?? "");
      else app.toast({ title: r.message || t(`code.finish.${action}` as Parameters<typeof t>[0]), tone: "success" });
      // The copy is gone: the session now works in the main checkout.
      if (r.removed) setBranch(null);
    } catch (e) {
      app.toast({ title: t("code.finish.failed"), body: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="worktree-footer">
      <Icon name="branch" size={14} />
      <span className="worktree-footer__text" title={t("code.isolatedHint")}>
        {t("code.isolated", { branch })}
      </span>
      <Menu
        label={t("code.finish")}
        placement="up"
        align="right"
        items={[
          { id: "merge", label: t("code.finish.merge"), icon: "merge", hint: t("code.finish.mergeHint"), onSelect: () => void finish("merge") },
          { id: "pr", label: t("code.finish.pr"), icon: "pullRequest", hint: t("code.finish.prHint"), onSelect: () => void finish("pr") },
          { id: "keep", label: t("code.finish.keep"), icon: "check", hint: t("code.finish.keepHint"), onSelect: () => void finish("keep") },
          { id: "discard", label: t("code.finish.discard"), icon: "trash", danger: true, separatorBefore: true, keepFocus: true, onSelect: () => setConfirm(true) },
        ]}
        trigger={(p) => (
          <button className="textbtn worktree-footer__btn" disabled={busy} {...p}>
            {busy ? t("common.working") : t("code.finish")}
            <Icon name="chevronDown" size={12} />
          </button>
        )}
      />
      {confirm && (
        <Dialog
          open
          title={t("code.finish.discardTitle")}
          onClose={() => setConfirm(false)}
          footer={
            <>
              <Button onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
              <Button
                variant="danger"
                onClick={() => {
                  setConfirm(false);
                  void finish("discard");
                }}
              >
                {t("code.finish.discard")}
              </Button>
            </>
          }
        >
          <p>{t("code.finish.discardBody", { branch })}</p>
        </Dialog>
      )}
      {pr != null && (
        <Dialog
          open
          title={t("code.finish.pr")}
          onClose={() => setPr(null)}
          footer={
            <>
              <Button onClick={() => void navigator.clipboard?.writeText(pr.split("\n").filter((l) => l.startsWith("  ")).map((l) => l.trim()).join("\n"))}>{t("code.finish.copyCommands")}</Button>
              <Button variant="primary" onClick={() => setPr(null)}>
                {t("common.close")}
              </Button>
            </>
          }
        >
          <pre className="worktree-footer__pr">{pr}</pre>
        </Dialog>
      )}
    </div>
  );
}
