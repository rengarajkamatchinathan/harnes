// New Code session (05 §3.1): folder (recent workspaces), agent, worktree option; the trust
// dialog on first open of a folder (D43) and pending definition-hash approvals.
import { useState } from "react";
import type { PendingDefinition } from "@genie/protocol";
import { Button, Dialog, Input, Select, Switch } from "@genie/ui";
import { t } from "../../i18n";
import { useApp } from "../context";

export function NewCodeSession({ onClose }: { onClose: () => void }) {
  const app = useApp();
  const [path, setPath] = useState(app.ui.recentWorkspaces[0] ?? "");
  const [worktree, setWorktree] = useState(false);
  const [agent, setAgent] = useState("");
  const [step, setStep] = useState<"form" | "trust" | "definitions">("form");
  const [defs, setDefs] = useState<PendingDefinition[]>([]);
  const [err, setErr] = useState<string | null>(null);

  const start = async () => {
    try {
      await app.createSession({ kind: "code", workspacePath: path, worktree, agent: agent || null });
      onClose();
    } catch (e) {
      setErr(e instanceof Error && e.message === "no_provider_configured" ? t("chat.emptyNoProvider") : app.describeError(e));
    }
  };

  const next = async () => {
    setErr(null);
    try {
      const trust = await app.client.trust.get({ path });
      if (trust.state === "unknown") return setStep("trust");
      if (trust.state === "trusted") {
        const pending = await app.client.trust.pending_definitions({ path }).catch(() => ({ items: [] }));
        if (pending.items.length) {
          setDefs(pending.items);
          return setStep("definitions");
        }
      }
    } catch {
      /* engines without trust.* still start sessions */
    }
    await start();
  };

  const decide = async (decision: "trust" | "once" | "deny") => {
    try {
      await app.client.trust.set({ path, decision });
      if (decision === "trust") {
        const pending = await app.client.trust.pending_definitions({ path }).catch(() => ({ items: [] }));
        if (pending.items.length) {
          setDefs(pending.items);
          return setStep("definitions");
        }
      }
      await start();
    } catch (e) {
      setErr(app.describeError(e));
    }
  };

  if (step === "trust")
    return (
      <Dialog
        open
        title={t("trust.title")}
        onClose={onClose}
        footer={
          <>
            <Button onClick={() => void decide("deny")}>{t("trust.deny")}</Button>
            <Button onClick={() => void decide("once")}>{t("trust.once")}</Button>
            <Button variant="primary" onClick={() => void decide("trust")}>
              {t("trust.trust")}
            </Button>
          </>
        }
      >
        <p>
          <code>{path}</code>
        </p>
        <p className="muted">{t("trust.body")}</p>
        {err && <div className="error-text">{err}</div>}
      </Dialog>
    );

  if (step === "definitions")
    return (
      <Dialog open title="Project definitions" onClose={onClose} width={720} footer={<Button variant="primary" onClick={() => void start()}>{t("code.start")}</Button>}>
        {defs.map((d) => (
          <div key={`${d.kind}:${d.name}`} className="stack" style={{ marginBottom: 12 }}>
            <strong>
              {d.kind} · {d.name}
            </strong>
            <pre className="code diff">{d.diff}</pre>
            <div className="row">
              <Button size="sm" variant="primary" onClick={() => void app.client.trust.approve_definition({ path, kind: d.kind, name: d.name, hash: d.hash, decision: "approve" }).then(() => setDefs((x) => x.filter((y) => y !== d)))}>
                {t("ask.approve")}
              </Button>
              <Button size="sm" onClick={() => void app.client.trust.approve_definition({ path, kind: d.kind, name: d.name, hash: d.hash, decision: "skip" }).then(() => setDefs((x) => x.filter((y) => y !== d)))}>
                {t("ask.skip")}
              </Button>
            </div>
          </div>
        ))}
      </Dialog>
    );

  return (
    <Dialog
      open
      title={t("code.newSession")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={!path.trim()} onClick={() => void next()}>
            {t("code.start")}
          </Button>
        </>
      }
    >
      <div className="stack">
        <label className="stack-tight">
          <span>{t("code.folder")}</span>
          <div className="row">
            <Input value={path} onChange={(e) => setPath(e.target.value)} placeholder={t("code.folderHint")} list="recent-workspaces" autoFocus />
            <Button icon="folder" onClick={() => void app.platform.pickFolder(t("code.folder")).then((p) => p && setPath(p), () => {})}>
              {t("code.chooseFolder")}
            </Button>
          </div>
          <datalist id="recent-workspaces">
            {app.ui.recentWorkspaces.map((w) => (
              <option key={w} value={w} />
            ))}
          </datalist>
        </label>
        <label className="stack-tight">
          <span>{t("code.agent")}</span>
          <Select value={agent} onChange={setAgent} options={[{ value: "", label: "default" }, { value: "plan", label: "plan" }, { value: "explore", label: "explore" }]} />
        </label>
        <Switch checked={worktree} onChange={setWorktree} label={t("code.worktree")} />
        {err && <div className="error-text">{err}</div>}
      </div>
    </Dialog>
  );
}
