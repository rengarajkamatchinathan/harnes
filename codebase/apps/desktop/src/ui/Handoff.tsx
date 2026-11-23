// Handoff files: "Share as handoff…" (export a redacted, read-only JSON snapshot of a session to
// Downloads), "Open handoff file…" (import) and the read-only replay view with "Continue in Genie".
import { useCallback, useRef, useState, type ReactNode } from "react";
import type { SessionSummary } from "@genie/protocol";
import { Badge, Button, Dialog, Icon, Switch } from "@genie/ui";
import { t } from "../i18n";
import type { App } from "../state/app";
import {
  buildHandoff,
  DEFAULT_HANDOFF_OPTIONS,
  handoffContinuePrompt,
  handoffFileName,
  parseHandoff,
  type Handoff,
  type HandoffMessage,
  type HandoffOptions,
} from "../state/handoff";
import type { PartView } from "../state/sessionModel";
import { useApp } from "./context";
import { TextPart } from "./chat/Markdown";

function waitLoaded(s: { loading: boolean; subscribe: (f: () => void) => () => void }): Promise<void> {
  if (!s.loading) return Promise.resolve();
  return new Promise((resolve) => {
    const off = s.subscribe(() => {
      if (!s.loading) {
        off();
        resolve();
      }
    });
  });
}

const baseName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;

/** Export a session as a handoff file (loads it first) → path saved to and redaction count. */
export async function exportHandoff(app: App, s: SessionSummary, opts: HandoffOptions): Promise<{ path: string; redactions: number }> {
  const store = app.session(s.session_id);
  await waitLoaded(store);
  const code = s.kind === "code";
  let diff: string | null = null;
  if (code && opts.includeDiff) diff = (await app.client.git.diff({ session_id: s.session_id }).catch(() => null))?.text ?? null;
  const project = s.project_id ? app.projects.find((p) => p.project_id === s.project_id) : undefined;
  const place = code && s.workspace_id ? { name: baseName(s.workspace_id), path: s.workspace_id } : project ? { name: project.name } : null;
  const h = buildHandoff(
    {
      title: s.title ?? store.model.info?.title,
      kind: s.kind,
      model: store.model.info?.model ?? null,
      place,
      messages: store.model.messages,
      tools: store.model.tools,
      diff,
    },
    { ...opts, includeDiff: code && opts.includeDiff },
  );
  const path = await app.platform.saveFile(handoffFileName(h.title), { text: JSON.stringify(h, null, 2) });
  return { path, redactions: h.redactions };
}

export function ShareHandoff({ s, onClose }: { s: SessionSummary; onClose: () => void }) {
  const app = useApp();
  const [opts, setOpts] = useState<HandoffOptions>(DEFAULT_HANDOFF_OPTIONS);
  const [busy, setBusy] = useState(false);
  const code = s.kind === "code";
  const set = (k: keyof HandoffOptions) => (v: boolean) => setOpts((o) => ({ ...o, [k]: v }));
  const run = async () => {
    setBusy(true);
    try {
      const r = await exportHandoff(app, s, opts);
      app.toast({ title: r.redactions ? t("handoff.savedRedacted", { path: r.path, n: r.redactions }) : t("handoff.saved", { path: r.path }), tone: "success" });
      onClose();
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      title={t("handoff.shareTitle")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" icon="download" disabled={busy} onClick={() => void run()}>
            {busy ? t("handoff.exporting") : t("handoff.export")}
          </Button>
        </>
      }
    >
      <div className="handoff-share">
        <Switch checked={opts.includeTools} onChange={set("includeTools")} label={t("handoff.optTools")} />
        <Switch checked={opts.includeTools && opts.includeToolOutput} disabled={!opts.includeTools} onChange={set("includeToolOutput")} label={t("handoff.optToolOutput")} />
        <Switch checked={opts.includeReasoning} onChange={set("includeReasoning")} label={t("handoff.optReasoning")} />
        {code && <Switch checked={opts.includeDiff} onChange={set("includeDiff")} label={t("handoff.optDiff")} />}
        <Switch checked={opts.fullPath} onChange={set("fullPath")} label={t("handoff.optPath")} />
        <p className="handoff-share__note muted">
          <Icon name="info" /> {t("handoff.note")}
        </p>
      </div>
    </Dialog>
  );
}

/** Hidden file input for "Open handoff file…": call the returned `open()` to pick a file. */
export function useHandoffImport(app: App): { input: ReactNode; open: () => void } {
  const ref = useRef<HTMLInputElement>(null);
  const onFile = (file: File | undefined) => {
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const r = parseHandoff(String(reader.result ?? ""));
      if (r.ok) app.openHandoff(r.handoff);
      else app.toast({ title: t("handoff.openFailed", { error: r.error }), tone: "danger" });
    };
    reader.onerror = () => app.toast({ title: t("handoff.openFailed", { error: String(reader.error ?? "") }), tone: "danger" });
    reader.readAsText(file);
  };
  const input = (
    <input
      ref={ref}
      type="file"
      accept=".json,application/json"
      hidden
      data-testid="handoff-file"
      onChange={(e) => {
        onFile(e.currentTarget.files?.[0]);
        e.currentTarget.value = "";
      }}
    />
  );
  const open = useCallback(() => ref.current?.click(), []);
  return { input, open };
}

const textPart = (id: string, text: string): PartView => ({ id, kind: "text", text, json: "", final: null, streaming: false });

function fmtDate(iso: string): string {
  const d = new Date(iso);
  return iso && !Number.isNaN(d.getTime()) ? d.toLocaleString() : "";
}

function ReplayMessage({ m, i }: { m: HandoffMessage; i: number }) {
  return (
    <div className={`handoff-msg handoff-msg--${m.role}`}>
      {m.reasoning && (
        <details className="handoff-fold">
          <summary>{t("handoff.reasoning")}</summary>
          <div className="handoff-fold__body">{m.reasoning}</div>
        </details>
      )}
      {m.tools.map((tool, j) => (
        <details key={j} className="handoff-fold handoff-tool">
          <summary>
            <span className="handoff-tool__name">{tool.name}</span>
            {tool.summary && tool.summary !== tool.name && <span className="handoff-tool__summary">: {tool.summary}</span>}
            {tool.outcome && <Badge tone={tool.outcome === "ok" ? "success" : tool.outcome === "error" ? "danger" : "neutral"}>{tool.outcome}</Badge>}
          </summary>
          {tool.result ? <pre className="handoff-fold__body handoff-pre">{tool.result}</pre> : <div className="handoff-fold__body muted">{t("handoff.noOutput")}</div>}
        </details>
      ))}
      {m.text &&
        (m.role === "user" ? (
          <div className="handoff-bubble">{m.text}</div>
        ) : (
          <TextPart part={textPart(`h${i}`, m.text)} />
        ))}
    </div>
  );
}

export function HandoffView() {
  const app = useApp();
  const h: Handoff | null = app.handoff;
  const [busy, setBusy] = useState(false);
  if (!h) {
    return (
      <div className="page">
        <div className="page__inner handoff">
          <p className="muted">{t("handoff.none")}</p>
          <Button onClick={() => app.setUi({ view: "main" })}>{t("palette.home")}</Button>
        </div>
      </div>
    );
  }
  const code = h.kind === "code";
  const proceed = async () => {
    setBusy(true);
    try {
      let workspacePath: string | undefined;
      if (code) {
        const p = await app.platform.pickFolder(t("code.folder"));
        if (!p) return;
        workspacePath = p;
      }
      const id = code ? await app.createSession({ kind: "code", workspacePath }) : await app.createSession({ kind: "chat" });
      const next = app.session(id);
      await waitLoaded(next);
      await next.send([{ type: "text", text: handoffContinuePrompt(h) }]);
      app.selectSession(id);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };
  const st = h.stats;
  return (
    <div className="page">
      <div className="page__inner handoff">
        <header className="handoff__head">
          <div className="handoff__titlerow">
            <h1 className="handoff__title">{h.title}</h1>
            <Badge tone="warning">{t("handoff.readOnly")}</Badge>
          </div>
          <div className="handoff__meta muted">
            <span>{code ? t("palette.codeMode") : t("palette.chatMode")}</span>
            {h.model && <span>{h.model}</span>}
            {h.place.name && <span title={h.place.path}>{h.place.name}</span>}
            {fmtDate(h.exported_at) && <span>{t("handoff.exportedAt", { date: fmtDate(h.exported_at) })}</span>}
          </div>
          <div className="handoff__meta muted">
            <span>{t("handoff.statMessages", { n: st.messages })}</span>
            <span>{t("handoff.statTools", { n: st.tools })}</span>
            {st.diffFiles !== undefined && (
              <span>
                {t("handoff.statFiles", { n: st.diffFiles })} <span className="handoff__add">+{st.added ?? 0}</span> <span className="handoff__del">−{st.removed ?? 0}</span>
              </span>
            )}
            {h.redactions > 0 && <span>{t("handoff.statRedactions", { n: h.redactions })}</span>}
          </div>
          <div className="handoff__actions">
            <Button variant="primary" icon="play" disabled={busy} onClick={() => void proceed()}>
              {t("handoff.continue")}
            </Button>
          </div>
        </header>
        <div className="handoff__messages">
          {h.messages.map((m, i) => (
            <ReplayMessage key={i} m={m} i={i} />
          ))}
        </div>
        {h.diff && (
          <section className="handoff__diff">
            <h2>
              {t("handoff.changes")} <span className="handoff__add">+{st.added ?? 0}</span> <span className="handoff__del">−{st.removed ?? 0}</span>
            </h2>
            <pre className="handoff-pre">{h.diff}</pre>
          </section>
        )}
      </div>
    </div>
  );
}
