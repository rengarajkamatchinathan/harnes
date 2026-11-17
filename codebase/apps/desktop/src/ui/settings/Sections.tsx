// Hand-built Settings sections (05 §4 table). Methods the engine doesn't serve show
// "Not available from this engine yet" instead of failing.
import { useState } from "react";
import type { MemoryItem } from "@genie/protocol";
import { applyAppearance, Badge, Banner, Button, Card, Dialog, Field, IconButton, Input, Segmented, Select, Switch, Textarea, type ChatFont, type Density, type ThemePref } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import { errorKind, type CliInstall } from "../../platform";
import { useApp, useAsync } from "../context";
import { DeleteAllData } from "./ManageSections";

function Unavailable({ error }: { error: unknown }) {
  const app = useApp();
  if (!error) return null;
  return <p className="muted">{errorKind(error) === "method_not_found" ? t("settings.notServed") : app.describeError(error)}</p>;
}

export function AppearanceSection() {
  const app = useApp();
  const a = app.ui.appearance;
  const set = (patch: Partial<typeof a>) => {
    const next = { ...a, ...patch };
    app.setUi({ appearance: next });
    applyAppearance(next);
    if (patch.theme) void app.client.config.set({ key: "theme", value: patch.theme, scope: "user" }).catch(() => {});
  };
  return (
    <div className="settings-group">
      <Field inline label={t("appearance.theme")}>
        <Segmented<ThemePref> label={t("appearance.theme")} value={a.theme} onChange={(theme) => set({ theme })} options={[{ value: "system", label: t("appearance.system") }, { value: "light", label: t("appearance.light") }, { value: "dark", label: t("appearance.dark") }]} />
      </Field>
      <Field inline label={t("appearance.chatFont")}>
        <Select value={a.chatFont} onChange={(v) => set({ chatFont: v as ChatFont })} options={(["default", "system", "dyslexia"] as const).map((f) => ({ value: f, label: t(`appearance.font.${f}`) }))} />
      </Field>
      <Field inline label={t("appearance.fontSize")}>
        <input className="range" type="range" min={12} max={30} value={a.fontSize} onChange={(e) => set({ fontSize: Number(e.target.value) })} aria-label={t("appearance.fontSize")} />
        <span className="range__value">{a.fontSize}px</span>
      </Field>
      <Field inline label={t("appearance.density")}>
        <Segmented<Density> label={t("appearance.density")} value={a.density} onChange={(density) => set({ density })} options={[{ value: "comfortable", label: t("appearance.comfortable") }, { value: "compact", label: t("appearance.compact") }]} />
      </Field>
    </div>
  );
}

export function GeneralExtras() {
  const app = useApp();
  const [report, setReport] = useState<unknown>(null);
  const [err, setErr] = useState<unknown>(null);
  return (
    <div className="settings-group">
      <Field inline label={t("settings.import")} description={t("settings.importHint")}>
        <Button onClick={() => void app.client.import.scan({}).then((r) => setReport(r.report), setErr)}>{t("settings.importScan")}</Button>
        {report != null && (
          <Button variant="primary" onClick={() => void app.client.import.apply({}).then((r) => (setReport(r.report), app.toast({ title: t("settings.imported"), tone: "success" })), setErr)}>
            {t("settings.importApply")}
          </Button>
        )}
      </Field>
      {report != null && <pre className="code">{JSON.stringify(report, null, 2)}</pre>}
      <Unavailable error={err} />
      <InstallCli />
      <Field inline label={t("settings.osNotifications")} description={t("settings.osNotificationsHint")}>
        <Switch checked={app.ui.osNotifications} onChange={(v) => app.setUi({ osNotifications: v })} aria-label={t("settings.osNotifications")} />
      </Field>
    </div>
  );
}

export function InstallCli() {
  const app = useApp();
  const [done, setDone] = useState<CliInstall | null>(null);
  const [busy, setBusy] = useState(false);
  const install = async () => {
    setBusy(true);
    try {
      setDone(await app.platform.installCli());
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Field
      inline
      label={t("settings.installCli")}
      description={
        done ? (
          <>
            {t("settings.installed", { path: done.path })} {!done.on_path && t("settings.addToPath", { dir: done.dir })}
          </>
        ) : (
          t("settings.installCliHint")
        )
      }
    >
      <Button disabled={busy} onClick={() => void install()}>
        {done ? t("settings.reinstall") : t("settings.install")}
      </Button>
    </Field>
  );
}

export function GenieSection() {
  const app = useApp();
  const mem = useAsync(() => app.client.memory.list({}), []);
  const [confirm, setConfirm] = useState(false);
  const [editing, setEditing] = useState<MemoryItem | null>(null);
  const [text, setText] = useState("");
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const items = mem.data?.items ?? [];
  return (
    <div className="settings-group">
      <div className="settings-subhead">
        <h3 className="section-title">{t("memory.saved")}</h3>
        {items.length > 0 && (
          <Button size="sm" variant="ghost" onClick={() => setConfirm(true)}>
            {t("memory.reset")}
          </Button>
        )}
      </div>
      <Unavailable error={mem.error} />
      {mem.data && !items.length && <p className="muted settings-note">{t("memory.empty")}</p>}
      {items.length > 0 && (
        <div className="list-box">
          {items.map((m) => (
            <div key={m.memory_id} className="list-box__row">
              <span className="grow">{m.text}</span>
              <Badge>{m.scope}</Badge>
              <IconButton icon="edit" size="sm" label={t("chat.edit")} onClick={() => (setEditing(m), setText(m.text))} />
              <IconButton icon="trash" size="sm" label={t("common.delete")} onClick={() => void app.client.memory.delete({ memory_id: m.memory_id }).then(mem.reload, fail)} />
            </div>
          ))}
        </div>
      )}
      <Dialog open={!!editing} title={t("chat.edit")} onClose={() => setEditing(null)} footer={<Button variant="primary" onClick={() => void app.client.memory.update({ memory_id: editing!.memory_id, text }).then(() => (setEditing(null), mem.reload()), fail)}>{t("common.save")}</Button>}>
        <Textarea value={text} onChange={(e) => setText(e.target.value)} />
      </Dialog>
      <Dialog
        open={confirm}
        title={t("memory.reset")}
        onClose={() => setConfirm(false)}
        footer={
          <>
            <Button onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
            <Button variant="danger" onClick={() => void app.client.memory.reset({}).then(() => (setConfirm(false), mem.reload()), fail)}>
              {t("memory.reset")}
            </Button>
          </>
        }
      >
        {t("memory.resetConfirm")}
      </Dialog>
    </div>
  );
}

export function CapabilitiesSection() {
  const app = useApp();
  const skills = useAsync(() => app.client.skill.list({}), []);
  const browser = useAsync(() => app.client.browser.status({}), []);
  const sites = useAsync(() => app.client.browser.sites.list({}), []);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const found = browser.data?.installed ?? [];
  return (
    <div className="settings-group">
      <h3 className="section-title">{t("capabilities.skills")}</h3>
      <Unavailable error={skills.error} />
      {skills.data && !skills.data.skills.length && <p className="muted settings-note">{t("capabilities.noSkills")}</p>}
      {skills.data?.skills.map((s) => (
        <Field key={s.id} inline label={s.name} description={[s.description, s.source].filter(Boolean).join(" · ")}>
          <Switch checked={s.enabled} aria-label={s.name} onChange={(v) => void app.client.skill.set_enabled({ id: s.id, enabled: v }).then(skills.reload, fail)} />
        </Field>
      ))}
      <h3 className="section-title">{t("capabilities.browser")}</h3>
      <Unavailable error={browser.error} />
      {browser.data && (
        <Field inline label={t("capabilities.browserLabel")} description={found.length ? found.map((b) => t("capabilities.browserFound", { kind: b.kind, version: b.version })).join(", ") : t("browser.none")}>
          <Button onClick={() => void app.client.browser.data.clear({ what: "all" }).then(() => app.toast({ title: t("capabilities.cleared"), tone: "success" }), fail)}>{t("capabilities.clearData")}</Button>
        </Field>
      )}
      {sites.data && sites.data.sites.length > 0 && (
        <>
          <h3 className="section-title">{t("capabilities.sites")}</h3>
          {sites.data.sites.map((s) => (
            <Field key={s.origin} inline label={<code>{s.origin}</code>}>
              <Select style={{ width: 140 }} value={s.state} onChange={(v) => void app.client.browser.sites.set({ origin: s.origin, state: v as typeof s.state }).then(sites.reload, fail)} options={["blocked", "ask", "read", "act"].map((x) => ({ value: x, label: x[0].toUpperCase() + x.slice(1) }))} />
            </Field>
          ))}
        </>
      )}
    </div>
  );
}

export function ConnectorsSection() {
  const app = useApp();
  const list = useAsync(() => app.client.mcp.list({}), []);
  const [logs, setLogs] = useState<{ name: string; lines: string[] } | null>(null);
  const [name, setName] = useState("");
  const [cmd, setCmd] = useState("");
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const add = () => {
    const isUrl = /^https?:\/\//.test(cmd);
    const [command, ...args] = cmd.split(/\s+/);
    const config = isUrl ? { type: "http", url: cmd } : { command, args };
    void app.client.mcp.add({ name, config, scope: "user" }).then(() => (setName(""), setCmd(""), list.reload()), fail);
  };
  return (
    <div className="stack">
      <h3 className="section-title">{t("connectors.title")}</h3>
      <Unavailable error={list.error} />
      {list.data && !list.data.servers.length && <p className="muted settings-note">{t("connectors.none")}</p>}
      {list.data?.servers.map((s) => (
        <Card key={s.name}>
          <div className="row">
            <strong>{s.name}</strong>
            <Badge tone={s.status === "connected" ? "success" : s.status === "failed" ? "danger" : s.status === "needs_auth" ? "warning" : "neutral"}>{s.status}</Badge>
            <span className="muted">{s.tools} tools · {s.scope}</span>
            <span className="grow" />
            {s.status === "needs_auth" && (
              <Button size="sm" onClick={() => void app.client.mcp.auth.start({ name: s.name }).then((r) => app.platform.openExternal(r.auth_url), fail)}>
                {t("connectors.signIn")}
              </Button>
            )}
            <Button size="sm" onClick={() => void app.client.mcp.logs({ name: s.name, tail: 200 }).then((r) => setLogs({ name: s.name, lines: r.lines }), fail)}>
              {t("connectors.logs")}
            </Button>
            <Switch checked={s.status !== "disabled"} aria-label={t("connectors.enabled")} onChange={(v) => void app.client.mcp.set_enabled({ name: s.name, enabled: v }).then(list.reload, fail)} />
            <Button size="sm" variant="ghost" onClick={() => void app.client.mcp.remove({ name: s.name, scope: s.scope }).then(list.reload, fail)}>
              {t("common.delete")}
            </Button>
          </div>
          {s.error && <div className="error-text">{s.error}</div>}
        </Card>
      ))}
      <Field label={t("connectors.add")} description={t("connectors.addHint")}>
        <div className="row">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("connectors.name")} aria-label={t("connectors.name")} style={{ maxWidth: 160 }} />
          <Input value={cmd} onChange={(e) => setCmd(e.target.value)} placeholder="npx my-server  or  https://…" aria-label="Command or URL" />
          <Button disabled={!name || !cmd} onClick={add}>
            {t("common.save")}
          </Button>
        </div>
      </Field>
      <Dialog open={!!logs} title={`${logs?.name ?? ""} · ${t("connectors.logs")}`} width={760} onClose={() => setLogs(null)} footer={<Button onClick={() => setLogs(null)}>{t("common.close")}</Button>}>
        <pre className="code">{logs?.lines.join("\n")}</pre>
      </Dialog>
    </div>
  );
}

export function UsageSection() {
  const app = useApp();
  const to = new Date();
  const from = new Date(to.getTime() - 30 * 86_400_000);
  const [group, setGroup] = useState<"day" | "provider" | "model" | "project">("provider");
  const rows = useAsync(() => app.client.usage.query({ from: from.toISOString(), to: to.toISOString(), group_by: [group] }), [group]);
  const [budget, setBudget] = useState("");
  const max = Math.max(1e-9, ...(rows.data?.rows ?? []).map((r) => r.cost.usd));
  return (
    <div className="settings-group">
      <div className="settings-subhead">
        <h3 className="section-title">{t("usage.last30")}</h3>
        <Segmented label={t("usage.groupBy")} value={group} onChange={setGroup} options={(["day", "provider", "model", "project"] as const).map((g) => ({ value: g, label: t(`usage.by.${g}`) }))} />
      </div>
      <Unavailable error={rows.error} />
      {rows.data?.rows.length === 0 && <p className="muted settings-note">{t("usage.none")}</p>}
      {!!rows.data?.rows.length && (
        <table className="table table--usage">
          <thead>
            <tr>
              <th>{t(`usage.by.${group}`)}</th>
              <th className="num">{t("usage.tokens")}</th>
              <th className="num">{t("usage.cost")}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rows.data.rows.map((r) => (
              <tr key={r.keys.join("/")}>
                <td>{r.keys.join(" · ") || t("usage.unassigned")}</td>
                <td className="num">{formatTokens((r.usage.input_tokens ?? 0) + (r.usage.output_tokens ?? 0))}</td>
                <td className="num">{formatCost(r.cost.usd)}</td>
                <td style={{ width: "40%" }}>
                  <div className="bar" style={{ width: `${(r.cost.usd / max) * 100}%` }} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Field inline label={t("usage.budget")} description={t("usage.budgetHint")}>
        <Input type="number" min={0} value={budget} onChange={(e) => setBudget(e.target.value)} placeholder={t("usage.noBudget")} aria-label={t("usage.budget")} style={{ width: 140 }} />
        <Button onClick={() => void app.client.usage.budget.set({ monthly_usd: budget ? Number(budget) : null }).then(() => app.toast({ title: t("settings.saved"), tone: "success" }), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>
          {t("common.save")}
        </Button>
      </Field>
    </div>
  );
}

export function PrivacySection() {
  const app = useApp();
  const [busy, setBusy] = useState(false);
  const exportAll = async () => {
    const path = await app.platform.pickSavePath("genie-export.zip", [{ name: "Zip", extensions: ["zip"] }]).catch(() => null);
    if (!path) return;
    setBusy(true);
    try {
      await app.client.export.all({ path });
      app.toast({ title: t("row.exported", { path }), tone: "success" });
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="settings-group">
      <Field inline label={t("privacy.exportAll")} description={t("privacy.exportHint")}>
        <Button disabled={busy} onClick={() => void exportAll()}>
          {t("privacy.export")}
        </Button>
      </Field>
      <DeleteAllData />
    </div>
  );
}

export function DeveloperSection() {
  const app = useApp();
  const status = useAsync(() => app.client.daemon.status({}), []);
  const problems = useAsync(() => app.client.config.diagnostics({}), []);
  const policy = useAsync(() => app.client.policy.effective({}), []);
  const [doctor, setDoctor] = useState<{ id: string; status: string; detail: string }[] | null>(null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  return (
    <div className="stack">
      <h3 className="section-title">{t("developer.daemon")}</h3>
      <Unavailable error={status.error} />
      {status.data && (
        <Card>
          <div>
            v{status.data.version} · {status.data.channel} · pid {status.data.pid} · up {Math.round(status.data.uptime_s / 60)} min · {status.data.sessions_running} running
          </div>
          <div className="muted">{status.data.socket}</div>
          <div className="muted">{status.data.clients.map((c) => `${c.client_kind} ${c.client_version}`).join(", ")}</div>
        </Card>
      )}
      <div className="row wrap">
        <Button icon="refresh" onClick={() => void app.client.daemon.restart({ stop_running: false }).catch(fail)}>
          {t("developer.restart")}
        </Button>
        <Button onClick={() => void app.client.doctor.run({}).then((r) => setDoctor(r.checks), fail)}>{t("developer.doctor")}</Button>
        <Button icon="copy" onClick={() => void app.client.diagnostics.collect({ include_logs: true }).then((r) => navigator.clipboard?.writeText(r.text)).then(() => app.toast({ title: t("chat.copied"), tone: "success" }), fail)}>
          {t("developer.copyDiagnostics")}
        </Button>
      </div>
      {doctor && (
        <table className="table">
          <tbody>
            {doctor.map((c) => (
              <tr key={c.id}>
                <td>{c.id}</td>
                <td>
                  <Badge tone={c.status === "ok" ? "success" : c.status === "warn" ? "warning" : "danger"}>{c.status}</Badge>
                </td>
                <td>{c.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <h3 className="section-title">{t("developer.problems")}</h3>
      {problems.data?.problems.length === 0 && <p className="muted settings-note">{t("developer.noProblems")}</p>}
      {problems.data?.problems.map((p, i) => (
        <Banner key={i} tone={p.severity === "error" ? "danger" : p.severity === "warning" ? "warning" : "info"}>
          {p.key && <code>{p.key}</code>} {p.message} {p.source && <span className="muted">({p.source})</span>}
        </Banner>
      ))}
      <h3 className="section-title">{t("developer.policy")}</h3>
      {policy.data && Object.keys(policy.data.values ?? {}).length > 0 ? <pre className="code">{JSON.stringify(policy.data.values, null, 2)}</pre> : <p className="muted settings-note">{t("developer.noPolicy")}</p>}
    </div>
  );
}

export function AccountSection() {
  const app = useApp();
  const notices = useAsync(() => app.platform.notices(), []);
  const [name, setName] = useState(app.ui.displayName);
  return (
    <div className="settings-group">
      <h3 className="section-title">{t("account.profile")}</h3>
      <Field inline label={t("account.callYou")} description={t("account.callYouHint")} htmlFor="display-name">
        <Input id="display-name" value={name} placeholder={app.info?.user_name || t("sidebar.you")} onChange={(e) => setName(e.target.value)} onBlur={() => app.setUi({ displayName: name.trim() })} />
      </Field>
      <h3 className="section-title">{t("account.about")}</h3>
      <Field inline label={t("account.appVersion")}>
        <span className="muted">{app.info?.app_version ?? "—"}</span>
      </Field>
      <Field inline label={t("account.engineVersion")}>
        <span className="muted">{app.status.engine_version ?? "—"}</span>
      </Field>
      <Field inline label={t("account.licenceLabel")}>
        <span className="muted">{t("account.licence")}</span>
      </Field>
      <h3 className="section-title">{t("account.notices")}</h3>
      {notices.loading ? (
        <p className="muted settings-note">{t("common.loading")}</p>
      ) : notices.data ? (
        <details className="notices">
          <summary>{t("account.showNotices")}</summary>
          <pre className="code notices__text">{notices.data}</pre>
        </details>
      ) : (
        <p className="muted settings-note">{t("account.noNotices")}</p>
      )}
    </div>
  );
}

