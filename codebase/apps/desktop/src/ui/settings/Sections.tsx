// Hand-built Settings sections (05 §4 table). Methods the engine doesn't serve show
// "Not available from this engine yet" instead of failing.
import { useState } from "react";
import type { MemoryItem } from "@genie/protocol";
import { applyAppearance, Badge, Banner, Button, Card, Dialog, Field, Input, Segmented, Select, Switch, Textarea, type ChatFont, type Density, type ThemePref } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import { errorKind } from "../../platform";
import { useApp, useAsync } from "../context";

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
    <div className="stack">
      <Field label={t("appearance.theme")}>
        <Segmented<ThemePref> label={t("appearance.theme")} value={a.theme} onChange={(theme) => set({ theme })} options={[{ value: "system", label: t("appearance.system") }, { value: "light", label: t("appearance.light") }, { value: "dark", label: t("appearance.dark") }]} />
      </Field>
      <Field label={t("appearance.chatFont")}>
        <Select value={a.chatFont} onChange={(v) => set({ chatFont: v as ChatFont })} options={(["default", "system", "dyslexia"] as const).map((f) => ({ value: f, label: t(`appearance.font.${f}`) }))} />
      </Field>
      <Field label={t("appearance.fontSize")} description={`${a.fontSize}px`}>
        <input type="range" min={12} max={30} value={a.fontSize} onChange={(e) => set({ fontSize: Number(e.target.value) })} aria-label={t("appearance.fontSize")} />
      </Field>
      <Field label={t("appearance.density")}>
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
    <div className="stack">
      <Field label="Import" description="Bring settings from Claude Code, Claude Desktop or OpenCode (D53).">
        <div className="row">
          <Button onClick={() => void app.client.import.scan({}).then((r) => setReport(r.report), setErr)}>Scan</Button>
          {report != null && (
            <Button variant="primary" onClick={() => void app.client.import.apply({}).then((r) => (setReport(r.report), app.toast({ title: "Imported", tone: "success" })), setErr)}>
              Import
            </Button>
          )}
        </div>
        {report != null && <pre className="code">{JSON.stringify(report, null, 2)}</pre>}
        <Unavailable error={err} />
      </Field>
      <Field label={'Install "genie" command in PATH'} description={t("settings.notServed")}>
        <Button disabled>Install</Button>
      </Field>
    </div>
  );
}

export function GenieSection() {
  const app = useApp();
  const mem = useAsync(() => app.client.memory.list({}), []);
  const [confirm, setConfirm] = useState(false);
  const [editing, setEditing] = useState<MemoryItem | null>(null);
  const [text, setText] = useState("");
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  return (
    <div className="stack">
      <h3 className="section-title">{t("memory.title")}</h3>
      <div className="row">
        <Switch checked label={t("memory.on")} onChange={(v) => void app.client.memory.set_state({ state: v ? "on" : "paused" }).catch(fail)} />
        <span className="grow" />
        <Button variant="danger" onClick={() => setConfirm(true)}>
          {t("memory.reset")}
        </Button>
      </div>
      <Unavailable error={mem.error} />
      {mem.data?.items.length === 0 && <p className="muted">{t("memory.empty")}</p>}
      {mem.data?.items.map((m) => (
        <Card key={m.memory_id}>
          <div className="row">
            <span className="grow">{m.text}</span>
            <Badge>{m.scope}</Badge>
            <Button size="sm" variant="ghost" onClick={() => (setEditing(m), setText(m.text))}>
              {t("chat.edit")}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void app.client.memory.delete({ memory_id: m.memory_id }).then(mem.reload, fail)}>
              {t("common.delete")}
            </Button>
          </div>
        </Card>
      ))}
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
  return (
    <div className="stack">
      <h3 className="section-title">{t("capabilities.skills")}</h3>
      <Unavailable error={skills.error} />
      {skills.data?.skills.map((s) => (
        <div key={s.id} className="row">
          <span className="grow">
            <strong>{s.name}</strong> <span className="muted">{s.description} · {s.source}</span>
          </span>
          <Switch checked={s.enabled} aria-label={s.name} onChange={(v) => void app.client.skill.set_enabled({ id: s.id, enabled: v }).then(skills.reload, fail)} />
        </div>
      ))}
      <h3 className="section-title">{t("capabilities.browser")}</h3>
      <Unavailable error={browser.error} />
      {browser.data && (browser.data.installed.length ? browser.data.installed.map((b) => <div key={b.path}>{t("capabilities.browserFound", { kind: b.kind, version: b.version })} <span className="muted">{b.path}</span></div>) : <p className="muted">{t("browser.none")}</p>)}
      {sites.data && sites.data.sites.length > 0 && (
        <>
          <h4>{t("capabilities.sites")}</h4>
          {sites.data.sites.map((s) => (
            <div key={s.origin} className="row">
              <code className="grow">{s.origin}</code>
              <Select style={{ width: "auto" }} value={s.state} onChange={(v) => void app.client.browser.sites.set({ origin: s.origin, state: v as typeof s.state }).then(sites.reload, fail)} options={["blocked", "ask", "read", "act"].map((x) => ({ value: x, label: x }))} />
            </div>
          ))}
        </>
      )}
      <div>
        <Button onClick={() => void app.client.browser.data.clear({ what: "all" }).then(() => app.toast({ title: t("capabilities.clearData"), tone: "success" }), fail)}>{t("capabilities.clearData")}</Button>
      </div>
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
      <Field label="Add custom server" description="A command (stdio) or an https:// URL.">
        <div className="row">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="name" aria-label="Server name" style={{ maxWidth: 160 }} />
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

export function PermissionsSection() {
  return (
    <div className="stack">
      <p className="muted">Rules above are edited per scope; saved “always” approvals appear in the allow list. Trusted folders are managed from Code mode (a list/revoke method is not served yet).</p>
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
    <div className="stack">
      <Segmented label="Group by" value={group} onChange={setGroup} options={(["day", "provider", "model", "project"] as const).map((g) => ({ value: g, label: g }))} />
      <Unavailable error={rows.error} />
      {rows.data?.rows.length === 0 && <p className="muted">{t("usage.none")}</p>}
      <table className="table">
        <tbody>
          {rows.data?.rows.map((r) => (
            <tr key={r.keys.join("/")}>
              <td>{r.keys.join(" · ") || "—"}</td>
              <td className="num">{formatTokens((r.usage.input_tokens ?? 0) + (r.usage.output_tokens ?? 0))}</td>
              <td className="num">{formatCost(r.cost.usd)}</td>
              <td style={{ width: "40%" }}>
                <div className="bar" style={{ width: `${(r.cost.usd / max) * 100}%` }} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <Field label={t("usage.budget")} description="Notifications at 80% and 100%.">
        <div className="row">
          <Input type="number" value={budget} onChange={(e) => setBudget(e.target.value)} style={{ maxWidth: 160 }} />
          <Button onClick={() => void app.client.usage.budget.set({ monthly_usd: budget ? Number(budget) : null }).then(() => app.toast({ title: t("settings.saved"), tone: "success" }), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>
            {t("common.save")}
          </Button>
        </div>
      </Field>
    </div>
  );
}

export function PrivacySection() {
  const app = useApp();
  const [path, setPath] = useState("");
  return (
    <div className="stack">
      <Field label={t("privacy.exportAll")} description="Writes an archive of every chat to a folder you choose.">
        <div className="row">
          <Input value={path} onChange={(e) => setPath(e.target.value)} placeholder="/path/to/export" aria-label={t("privacy.exportAll")} />
          <Button disabled={!path} onClick={() => void app.client.export.all({ path }).then(() => app.toast({ title: t("row.exported", { path }), tone: "success" }), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>
            {t("privacy.exportAll")}
          </Button>
        </div>
      </Field>
      <Field label={t("privacy.deleteAll")} description={t("settings.notServed")}>
        <Button variant="danger" disabled>
          {t("privacy.deleteAll")}
        </Button>
      </Field>
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
      {problems.data?.problems.length === 0 && <p className="muted">{t("developer.noProblems")}</p>}
      {problems.data?.problems.map((p, i) => (
        <Banner key={i} tone={p.severity === "error" ? "danger" : p.severity === "warning" ? "warning" : "info"}>
          {p.key && <code>{p.key}</code>} {p.message} {p.source && <span className="muted">({p.source})</span>}
        </Banner>
      ))}
      <h3 className="section-title">{t("developer.policy")}</h3>
      <pre className="code">{JSON.stringify(policy.data?.values ?? {}, null, 2)}</pre>
    </div>
  );
}

export function AccountSection() {
  const app = useApp();
  return (
    <div className="stack">
      <Card>
        <strong>{t("account.about")}</strong>
        <div>{t("account.version", { app: app.info?.app_version ?? "?", engine: app.status.engine_version ?? "?" })}</div>
        <div className="muted">{t("account.licence")}</div>
        <div className="muted">{t("account.notices")}</div>
      </Card>
    </div>
  );
}
