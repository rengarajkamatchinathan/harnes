// "Ready to commit" checklist under the latest reply of a Code session (after a turn that
// changed files): files / tests / lint / uncommitted, then Commit… and View diff.
import { useMemo, useState } from "react";
import { Button, Dialog, Icon, IconButton, Textarea } from "@genie/ui";
import { t } from "../../i18n";
import type { SessionStore } from "../../state/session";
import { useAppStatic, useAsync } from "../context";
import { withTrailer } from "./commitLinks";
import { type CheckState, commitMessage, diffStats, lastTurn, turnChecks } from "./commitReady";
import { focusDiff } from "./diffFocus";

function Mark({ state }: { state: CheckState | "pending" }) {
  if (state === "ok")
    return (
      <span className="ready__mark ready__mark--ok">
        <Icon name="check" title="passed" />
      </span>
    );
  if (state === "failed")
    return (
      <span className="ready__mark ready__mark--failed">
        <Icon name="x" title="failed" />
      </span>
    );
  return (
    <span className={`ready__mark ready__mark--${state === "pending" ? "pending" : "none"}`} aria-hidden>
      –
    </span>
  );
}

const hidden = new Set<string>();

export function ReadyToCommit({ store }: { store: SessionStore }) {
  const app = useAppStatic();
  const model = store.model;
  const sid = store.id;
  const last = model.messages[model.messages.length - 1];
  const key = last ? `${last.id}:${last.rev}` : "";
  const turn = useMemo(() => lastTurn(model), [key]); // eslint-disable-line react-hooks/exhaustive-deps
  const checks = useMemo(() => turnChecks(turn.calls), [turn]);
  const [n, setN] = useState(0);
  const status = useAsync(() => app.client.git.status({ session_id: sid }), [sid, key, n]);
  const diff = useAsync(() => app.client.git.diff({ session_id: sid }), [sid, key, n]);
  const stats = useMemo(() => diffStats(diff.data?.text ?? ""), [diff.data]);
  const [open, setOpen] = useState(false);
  const [dialog, setDialog] = useState(false);
  const [message, setMessage] = useState("");
  const [committing, setCommitting] = useState(false);
  const [, setDismissed] = useState(0);

  const files = status.data?.files ?? [];
  // Shown only while there is something to commit (and the repo answered git.status).
  if (!last || hidden.has(last.id) || status.error || !files.length) return null;

  const byPath = new Map(stats.files.map((f) => [f.path, f]));
  const openChanges = (path?: string) => {
    if (path) focusDiff(path);
    app.setUi({ panelOpen: true, panelTab: "changes" });
  };
  const ask = (text: string) => void store.send([{ type: "text", text }]).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  const startCommit = () => {
    setMessage(commitMessage({ title: model.info?.title, prompt: turn.prompt, files: files.map((f) => f.path) }));
    setDialog(true);
  };
  const commit = async () => {
    setCommitting(true);
    try {
      const r = await app.client.git.commit({ session_id: sid, message: withTrailer(message.trim(), sid), then: null });
      app.toast({ title: t("ready.committed", { hash: r.commit.slice(0, 8) }), body: r.note ?? r.pr_url ?? undefined, tone: "success", link: null });
      setDialog(false);
      setN((x) => x + 1);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setCommitting(false);
    }
  };

  const check = (kind: "tests" | "lint") => {
    const state = checks[kind];
    const cmd = kind === "tests" ? checks.testCommand : checks.lintCommand;
    const label = kind === "tests" ? { ok: t("ready.testsOk"), failed: t("ready.testsFailed"), not_run: t("ready.testsNotRun") }[state] : { ok: t("ready.lintOk"), failed: t("ready.lintFailed"), not_run: t("ready.lintNotRun") }[state];
    return (
      <li className="ready__row">
        <Mark state={state} />
        <span className="ready__label" title={cmd}>
          {label}
          {cmd && <code className="ready__cmd">{cmd}</code>}
        </span>
        {state === "not_run" && (
          <Button size="sm" variant="ghost" onClick={() => ask(kind === "tests" ? t("ready.runTestsPrompt") : t("ready.runLintPrompt"))}>
            {kind === "tests" ? t("ready.runTests") : t("ready.runLint")}
          </Button>
        )}
        {state === "failed" && (
          <Button size="sm" variant="ghost" onClick={() => ask(kind === "tests" ? t("ready.fixTestsPrompt") : t("ready.fixLintPrompt"))}>
            {t("ready.fix")}
          </Button>
        )}
      </li>
    );
  };

  return (
    <section className="ready" aria-label={t("ready.title")}>
      <header className="ready__head">
        <Icon name="branch" />
        <strong>{t("ready.title")}</strong>
        <span className="grow" />
        <IconButton
          icon="x"
          size="sm"
          label={t("ready.dismiss")}
          onClick={() => {
            hidden.add(last.id);
            setDismissed((x) => x + 1);
          }}
        />
      </header>
      <ul className="ready__list">
        <li className="ready__row">
          <Mark state="ok" />
          <button className="ready__label ready__toggle" aria-expanded={open} aria-label={t("ready.showFiles")} onClick={() => setOpen((o) => !o)}>
            <Icon name={open ? "chevronDown" : "chevronRight"} />
            {files.length === 1 ? t("ready.filesOne") : t("ready.files", { n: files.length })}
            {(stats.add > 0 || stats.del > 0) && (
              <span className="ready__stat">
                <span className="ready__add">+{stats.add}</span> <span className="ready__del">−{stats.del}</span>
              </span>
            )}
          </button>
        </li>
        {open && (
          <li className="ready__files">
            <ul>
              {files.map((f) => {
                const s = byPath.get(f.path);
                return (
                  <li key={f.path}>
                    <button className="ready__file" onClick={() => openChanges(f.path)} title={f.path}>
                      <span className={`ready__change ready__change--${f.change}`}>{f.change === "added" ? "A" : f.change === "deleted" ? "D" : "M"}</span>
                      <code>{f.path}</code>
                      {s && !s.binary && (
                        <span className="ready__stat">
                          <span className="ready__add">+{s.add}</span> <span className="ready__del">−{s.del}</span>
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          </li>
        )}
        {check("tests")}
        {check("lint")}
        <li className="ready__row">
          <Mark state="pending" />
          <span className="ready__label">
            {files.length === 1 ? t("ready.uncommittedOne") : t("ready.uncommitted", { n: files.length })}
            {status.data?.branch && <span className="muted"> {t("ready.onBranch", { branch: status.data.branch })}</span>}
          </span>
        </li>
      </ul>
      <div className="ready__actions">
        <Button size="sm" variant="primary" icon="check" onClick={startCommit}>
          {t("ready.commit")}
        </Button>
        <Button size="sm" icon="branch" onClick={() => openChanges()}>
          {t("ready.viewDiff")}
        </Button>
      </div>
      <Dialog
        open={dialog}
        title={t("ready.dialogTitle")}
        onClose={() => !committing && setDialog(false)}
        width={520}
        footer={
          <>
            <Button onClick={() => setDialog(false)} disabled={committing}>
              {t("common.cancel")}
            </Button>
            <Button variant="primary" onClick={() => void commit()} disabled={committing || !message.trim()}>
              {t("code.commit")}
            </Button>
          </>
        }
      >
        <div className="stack">
          <Textarea className="ready__message" value={message} onChange={(e) => setMessage(e.target.value)} rows={8} aria-label={t("code.commitMessage")} spellCheck={false} />
          <div className="muted ready__hint">{t("ready.dialogHint")}</div>
        </div>
      </Dialog>
    </section>
  );
}
