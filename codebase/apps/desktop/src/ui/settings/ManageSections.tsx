// Settings that manage stored state (05 §4): Permissions (saved rules and "always"
// approvals per scope with remove, trusted folders with revoke — D43, 06 §5.4) and
// Privacy → Delete all data (two-step, type-to-confirm, 04 §8).
import { useState } from "react";
import type { DeleteAllBeginResult, PermissionRule, TrustedFolder } from "@genie/protocol";
import { Badge, Banner, Button, Dialog, Icon, Input, Select, SettingRow, SettingsGroup } from "@genie/ui";
import { t } from "../../i18n";
import { errorKind } from "../../platform";
import { formatBytes } from "../../state/attachments";
import { useApp, useAsync } from "../context";

const CLASS_TONE = { allow: "success", ask: "warning", deny: "danger", unknown: "neutral" } as const;

export function PermissionsSection() {
  const app = useApp();
  const [cwd, setCwd] = useState("");
  const rules = useAsync(() => app.client.permissions.rules.list({ cwd: cwd || null }), [cwd]);
  const trust = useAsync(() => app.client.trust.list({}), []);
  const fail = (e: unknown) => app.toast({ title: errorKind(e) === "locked" ? t("permissions.managed") : app.describeError(e), tone: "danger" });

  const remove = (r: PermissionRule) =>
    void app.client.permissions.rules
      .remove({ rule: r.rule, class: r.class, scope: r.scope, cwd: cwd || null })
      .then(rules.reload, fail);
  const revoke = (f: TrustedFolder) => void app.client.trust.set({ path: f.path, decision: "revoke" }).then(trust.reload, fail);

  const list: PermissionRule[] = rules.data?.rules ?? [];
  return (
    <>
    <SettingsGroup title={t("permissions.rules")}>
      {app.ui.recentWorkspaces.length > 0 && (
        <SettingRow label={t("permissions.folder")}>
          <Select value={cwd} onChange={setCwd} aria-label={t("permissions.folder")} options={[{ value: "", label: t("permissions.allFolders") }, ...app.ui.recentWorkspaces.map((w) => ({ value: w, label: w }))]} />
        </SettingRow>
      )}
      {rules.error ? <p className="muted">{app.describeError(rules.error)}</p> : null}
      {rules.data && !list.length && <p className="muted">{t("permissions.none")}</p>}
      {list.length > 0 && (
        <table className="table">
          <tbody>
            {list.map((r) => (
              <tr key={`${r.scope}/${r.class}/${r.rule}`}>
                <td>
                  <Badge tone={CLASS_TONE[r.class] ?? "neutral"}>{t(`permissions.${r.class === "unknown" ? "ask" : r.class}` as "permissions.allow")}</Badge>
                </td>
                <td>
                  <code>{r.rule}</code>
                </td>
                <td className="muted">
                  {r.scope}
                  {r.source ? ` · ${r.source}` : ""}
                </td>
                <td className="num">
                  {r.locked ? (
                    <span className="muted" title={t("permissions.managed")}>
                      <Icon name="lock" /> {t("permissions.managed")}
                    </span>
                  ) : (
                    <Button size="sm" variant="ghost" onClick={() => remove(r)}>
                      {t("common.delete")}
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </SettingsGroup>
    <SettingsGroup title={t("permissions.trusted")}>
      {trust.error ? <p className="muted">{app.describeError(trust.error)}</p> : null}
      {trust.data && !trust.data.items.length && <p className="muted">{t("permissions.noTrusted")}</p>}
      {(trust.data?.items ?? []).map((f) => (
        <SettingRow
          key={f.path}
          label={<code>{f.path}</code>}
          extra={<Badge tone={f.decision === "trust" ? "success" : "danger"}>{f.decision === "trust" ? t("permissions.trustedBadge") : t("permissions.blockedBadge")}</Badge>}
          description={[f.approvals ? t("permissions.approvals", { n: f.approvals }) : "", new Date(f.decided_at).toLocaleDateString()].filter(Boolean).join(" · ")}
        >
          <Button size="sm" onClick={() => revoke(f)}>
            {t("permissions.revoke")}
          </Button>
        </SettingRow>
      ))}
    </SettingsGroup>
    </>
  );
}

/** Privacy → Delete all data: `data.delete_all.begin` shows what goes, then type-to-confirm. */
export function DeleteAllData() {
  const app = useApp();
  const [plan, setPlan] = useState<DeleteAllBeginResult | null>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const begin = () => {
    setTyped("");
    void app.client.data.delete_all.begin({}).then(setPlan, fail);
  };
  const confirm = async () => {
    if (!plan) return;
    setBusy(true);
    try {
      const r = await app.client.data.delete_all.confirm({ token: plan.token });
      setPlan(null);
      await app.reloadAfterDeleteAll();
      app.toast({ title: t("privacy.deleteAllDone", { n: r.sessions_deleted }), tone: "success" });
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <SettingRow label={t("privacy.deleteAll")} description={t("privacy.deleteAllHint")}>
      <Button size="sm" variant="danger" onClick={begin}>
        {t("privacy.deleteAllShort")}
      </Button>
      <Dialog
        open={!!plan}
        title={t("privacy.deleteAll")}
        onClose={() => setPlan(null)}
        footer={
          <>
            <Button onClick={() => setPlan(null)}>{t("common.cancel")}</Button>
            <Button variant="danger" disabled={busy || typed.trim() !== "DELETE"} onClick={() => void confirm()}>
              {t("privacy.deleteAll")}
            </Button>
          </>
        }
      >
        {plan && (
          <div className="stack">
            <Banner tone="danger">{t("privacy.deleteAllCounts", { sessions: plan.sessions, projects: plan.projects, memories: plan.memories, artifacts: plan.artifacts, bytes: formatBytes(plan.blob_bytes) })}</Banner>
            <p className="muted">{plan.clears_audit ? t("privacy.deleteAllAudit") : t("privacy.deleteAllKeepAudit")}</p>
            <label className="stack-tight">
              <span>{t("privacy.deleteAllType")}</span>
              <Input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus aria-label={t("privacy.deleteAllType")} />
            </label>
          </div>
        )}
      </Dialog>
    </SettingRow>
  );
}
