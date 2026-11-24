// Hand-built Settings sections (05 §4 table), laid out as titled cards of rows. Methods the
// engine doesn't serve show "Not available from this engine yet" instead of failing.
import { useState, type ReactNode } from "react";
import type { MemoryItem } from "@genie/protocol";
import { applyAppearance, Badge, Banner, Button, Dialog, IconButton, Input, Segmented, Select, SettingRow, SettingsGroup, Stepper, Switch, Textarea, type ChatFont, type Density, type ThemePref } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import { errorKind, type CliInstall } from "../../platform";
import { useApp, useAsync } from "../context";
import { DeleteAllData } from "./ManageSections";
import { OverallBudget } from "../BudgetGuard";
import { stepZoom, ZOOM_STEPS } from "./schema";

function Unavailable({ error }: { error: unknown }) {
  const app = useApp();
  if (!error) return null;
  return <p className="muted">{errorKind(error) === "method_not_found" ? t("settings.notServed") : app.describeError(error)}</p>;
}

export function GeneralSection({ updates, onAccount }: { /** The schema-generated Updates card. */ updates: ReactNode; onAccount: () => void }) {
  const app = useApp();
  const [report, setReport] = useState<unknown>(null);
  const [err, setErr] = useState<unknown>(null);
  return (
    <>
      <SettingsGroup>
        <SettingRow label={t("settings.section.account")} description={t("settings.accountHint")}>
          <Button size="sm" onClick={onAccount}>
            {t("settings.manage")}
          </Button>
        </SettingRow>
      </SettingsGroup>
      {updates}
      <SettingsGroup title={t("settings.group.notifications")}>
        <SettingRow label={t("settings.osNotifications")} description={t("settings.osNotificationsHint")}>
          <Switch checked={app.ui.osNotifications} onChange={(v) => app.setUi({ osNotifications: v })} aria-label={t("settings.osNotifications")} />
        </SettingRow>
        <SettingRow label={t("settings.completionSound")} description={t("settings.completionSoundHint")}>
          <Switch checked={app.ui.completionSound} onChange={(v) => (app.setUi({ completionSound: v }), v && app.chime())} aria-label={t("settings.completionSound")} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title={t("settings.group.tools")}>
        <SettingRow label={t("settings.import")} description={t("settings.importHint")}>
          <Button size="sm" onClick={() => void app.client.import.scan({}).then((r) => setReport(r.report), setErr)}>
            {t("settings.importScan")}
          </Button>
          {report != null && (
            <Button size="sm" variant="primary" onClick={() => void app.client.import.apply({}).then((r) => (setReport(r.report), app.toast({ title: t("settings.imported"), tone: "success" })), setErr)}>
              {t("settings.importApply")}
            </Button>
          )}
        </SettingRow>
        {report != null && <pre className="code">{JSON.stringify(report, null, 2)}</pre>}
        {err != null && <Unavailable error={err} />}
        <InstallCli />
      </SettingsGroup>
    </>
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
    <SettingRow
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
      <Button size="sm" disabled={busy} onClick={() => void install()}>
        {done ? t("settings.reinstall") : t("settings.install")}
      </Button>
    </SettingRow>
  );
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
    <>
      <SettingsGroup>
        <SettingRow label={t("appearance.theme")} description={t("appearance.themeHint")} htmlFor="set-theme">
          <Select id="set-theme" value={a.theme} onChange={(v) => set({ theme: v as ThemePref })} options={(["system", "light", "dark"] as const).map((v) => ({ value: v, label: t(`appearance.${v}`) }))} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title={t("appearance.group.chat")}>
        <SettingRow label={t("appearance.density")} description={t("appearance.densityHint")} htmlFor="set-density">
          <Select id="set-density" value={a.density} onChange={(v) => set({ density: v as Density })} options={(["comfortable", "compact"] as const).map((v) => ({ value: v, label: t(`appearance.${v}`) }))} />
        </SettingRow>
        <SettingRow label={t("appearance.wrap")} description={t("appearance.wrapHint")}>
          <Switch checked={app.ui.codeWrap} onChange={(v) => app.setUi({ codeWrap: v })} aria-label={t("appearance.wrap")} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title={t("appearance.group.typography")}>
        <SettingRow label={t("appearance.zoom")} description={t("appearance.zoomHint")}>
          <Stepper label={t("appearance.zoom")} value={app.ui.zoom} min={ZOOM_STEPS[0]} max={ZOOM_STEPS[ZOOM_STEPS.length - 1]} next={stepZoom} format={(v) => `${Math.round(v * 100)}%`} onChange={(zoom) => app.setUi({ zoom })} />
        </SettingRow>
        <SettingRow label={t("appearance.fontSize")} description={t("appearance.fontSizeHint")}>
          <Stepper label={t("appearance.fontSize")} value={a.fontSize} min={12} max={30} onChange={(fontSize) => set({ fontSize })} />
        </SettingRow>
        <SettingRow label={t("appearance.chatFont")} description={t("appearance.chatFontHint")} htmlFor="set-chat-font">
          <Select id="set-chat-font" value={a.chatFont} onChange={(v) => set({ chatFont: v as ChatFont })} options={(["default", "poppins", "dyslexia"] as const).map((f) => ({ value: f, label: t(`appearance.font.${f}`) }))} />
        </SettingRow>
      </SettingsGroup>
    </>
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
    <>
      <SettingsGroup
        title={t("memory.saved")}
        action={
          items.length > 0 && (
            <Button size="sm" variant="ghost" onClick={() => setConfirm(true)}>
              {t("memory.reset")}
            </Button>
          )
        }
      >
        {mem.error ? <Unavailable error={mem.error} /> : null}
        {mem.data && !items.length && <p className="muted">{t("memory.empty")}</p>}
        {items.map((m) => (
          <SettingRow key={m.memory_id} label={m.text} extra={<Badge>{m.scope}</Badge>}>
            <IconButton icon="edit" size="sm" label={t("chat.edit")} onClick={() => (setEditing(m), setText(m.text))} />
            <IconButton icon="trash" size="sm" label={t("common.delete")} onClick={() => void app.client.memory.delete({ memory_id: m.memory_id }).then(mem.reload, fail)} />
          </SettingRow>
        ))}
        {!mem.data && !mem.error && <p className="muted">{t("common.loading")}</p>}
      </SettingsGroup>
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
    </>
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
    <>
      <SettingsGroup title={t("capabilities.skills")}>
        {skills.error ? <Unavailable error={skills.error} /> : null}
        {skills.data && !skills.data.skills.length && <p className="muted">{t("capabilities.noSkills")}</p>}
        {!skills.data && !skills.error && <p className="muted">{t("common.loading")}</p>}
        {skills.data?.skills.map((s) => (
          <SettingRow key={s.id} label={s.name} description={[s.description, s.source].filter(Boolean).join(" · ")}>
            <Switch checked={s.enabled} aria-label={s.name} onChange={(v) => void app.client.skill.set_enabled({ id: s.id, enabled: v }).then(skills.reload, fail)} />
          </SettingRow>
        ))}
      </SettingsGroup>
      <SettingsGroup title={t("capabilities.browser")}>
        {browser.error ? <Unavailable error={browser.error} /> : null}
        {!browser.data && !browser.error && <p className="muted">{t("common.loading")}</p>}
        {browser.data && (
          <SettingRow label={t("capabilities.browserLabel")} description={found.length ? found.map((b) => t("capabilities.browserFound", { kind: b.kind, version: b.version })).join(", ") : t("browser.none")}>
            <Button size="sm" onClick={() => void app.client.browser.data.clear({ what: "all" }).then(() => app.toast({ title: t("capabilities.cleared"), tone: "success" }), fail)}>
              {t("capabilities.clearData")}
            </Button>
          </SettingRow>
        )}
      </SettingsGroup>
      {sites.data && sites.data.sites.length > 0 && (
        <SettingsGroup title={t("capabilities.sites")}>
          {sites.data.sites.map((s) => (
            <SettingRow key={s.origin} label={<code>{s.origin}</code>}>
              <Select value={s.state} onChange={(v) => void app.client.browser.sites.set({ origin: s.origin, state: v as typeof s.state }).then(sites.reload, fail)} options={["blocked", "ask", "read", "act"].map((x) => ({ value: x, label: x[0].toUpperCase() + x.slice(1) }))} />
            </SettingRow>
          ))}
        </SettingsGroup>
      )}
    </>
  );
}

const MCP_TONE = { connected: "success", failed: "danger", needs_auth: "warning" } as const;

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
    <>
      <SettingsGroup title={t("connectors.title")}>
        {list.error ? <Unavailable error={list.error} /> : null}
        {list.data && !list.data.servers.length && <p className="muted">{t("connectors.none")}</p>}
        {!list.data && !list.error && <p className="muted">{t("common.loading")}</p>}
        {list.data?.servers.map((s) => (
          <SettingRow
            key={s.name}
            label={s.name}
            extra={<Badge tone={MCP_TONE[s.status as keyof typeof MCP_TONE] ?? "neutral"}>{s.status}</Badge>}
            description={
              <>
                {s.tools} tools · {s.scope}
                {s.error && <span className="error-text settings-row-error">{s.error}</span>}
              </>
            }
          >
            {s.status === "needs_auth" && (
              <Button size="sm" onClick={() => void app.client.mcp.auth.start({ name: s.name }).then((r) => app.platform.openExternal(r.auth_url), fail)}>
                {t("connectors.signIn")}
              </Button>
            )}
            <Button size="sm" variant="ghost" onClick={() => void app.client.mcp.logs({ name: s.name, tail: 200 }).then((r) => setLogs({ name: s.name, lines: r.lines }), fail)}>
              {t("connectors.logs")}
            </Button>
            <IconButton icon="trash" size="sm" label={t("common.delete")} onClick={() => void app.client.mcp.remove({ name: s.name, scope: s.scope }).then(list.reload, fail)} />
            <Switch checked={s.status !== "disabled"} aria-label={t("connectors.enabled")} onChange={(v) => void app.client.mcp.set_enabled({ name: s.name, enabled: v }).then(list.reload, fail)} />
          </SettingRow>
        ))}
      </SettingsGroup>
      <SettingsGroup title={t("connectors.add")}>
        <SettingRow label={t("connectors.add")} description={t("connectors.addHint")} stacked>
          <div className="row">
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("connectors.name")} aria-label={t("connectors.name")} style={{ maxWidth: 160 }} />
            <Input value={cmd} onChange={(e) => setCmd(e.target.value)} placeholder="npx my-server  or  https://…" aria-label="Command or URL" />
            <Button disabled={!name || !cmd} onClick={add}>
              {t("common.save")}
            </Button>
          </div>
        </SettingRow>
      </SettingsGroup>
      <Dialog open={!!logs} title={`${logs?.name ?? ""} · ${t("connectors.logs")}`} width={760} onClose={() => setLogs(null)} footer={<Button onClick={() => setLogs(null)}>{t("common.close")}</Button>}>
        <pre className="code">{logs?.lines.join("\n")}</pre>
      </Dialog>
    </>
  );
}

/** All usage at a glance: today, last 7 days, last 30 days and all time (tokens in/out, cost). */
function UsageOverview() {
  const app = useApp();
  const rows = useAsync(() => app.client.usage.query({ from: "2000-01-01T00:00:00Z", to: new Date().toISOString(), group_by: ["day"] }), []);
  if (!rows.data?.rows.length) return null;
  const day = (offset: number) => new Date(Date.now() - offset * 86_400_000).toISOString().slice(0, 10);
  const sum = (sinceDay: string | null) => {
    const s = { input: 0, output: 0, usd: 0, days: 0 };
    for (const r of rows.data!.rows) {
      const d = r.keys[0] ?? "";
      if (sinceDay && d < sinceDay) continue;
      s.input += r.usage.input_tokens ?? 0;
      s.output += r.usage.output_tokens ?? 0;
      s.usd += r.cost.usd;
      s.days += 1;
    }
    return s;
  };
  const cards = [
    { k: t("usage.today"), s: sum(day(0)) },
    { k: t("usage.last7"), s: sum(day(6)) },
    { k: t("usage.last30short"), s: sum(day(29)) },
    { k: t("usage.allTime"), s: sum(null) },
  ];
  return (
    <SettingsGroup title={t("usage.overview")} className="g-sgroup--bare">
      <div className="usage-cards">
        {cards.map(({ k, s }) => (
          <div key={k} className="usage-card">
            <span className="usage-card__label">{k}</span>
            <strong className="usage-card__value">{formatTokens(s.input + s.output)}</strong>
            <span className="usage-card__meta">{t("usage.inOut", { input: formatTokens(s.input), output: formatTokens(s.output) })}</span>
            {s.usd > 0 && <span className="usage-card__meta">{formatCost(s.usd)}</span>}
          </div>
        ))}
      </div>
    </SettingsGroup>
  );
}

export function UsageSection() {
  const app = useApp();
  const to = new Date();
  const from = new Date(to.getTime() - 30 * 86_400_000);
  const [group, setGroup] = useState<"day" | "provider" | "model" | "project">("provider");
  const rows = useAsync(() => app.client.usage.query({ from: from.toISOString(), to: to.toISOString(), group_by: [group] }), [group]);
  const [budget, setBudget] = useState("");
  // Bars follow cost, or tokens when every model is free (e.g. a company deployment).
  const tokensOf = (r: { usage: { input_tokens?: number; output_tokens?: number } }) => (r.usage.input_tokens ?? 0) + (r.usage.output_tokens ?? 0);
  const byCost = (rows.data?.rows ?? []).some((r) => r.cost.usd > 0);
  const barOf = (r: NonNullable<typeof rows.data>["rows"][number]) => (byCost ? r.cost.usd : tokensOf(r));
  const max = Math.max(1e-9, ...(rows.data?.rows ?? []).map(barOf));
  return (
    <>
      <UsageOverview />
      <OverallBudget />
      <SettingsGroup title={t("usage.last30")} action={<Segmented label={t("usage.groupBy")} value={group} onChange={setGroup} options={(["day", "provider", "model", "project"] as const).map((g) => ({ value: g, label: t(`usage.by.${g}`) }))} />}>
        {rows.error ? <Unavailable error={rows.error} /> : null}
        {rows.data?.rows.length === 0 && <p className="muted">{t("usage.none")}</p>}
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
                    <div className="bar" style={{ width: `${(barOf(r) / max) * 100}%` }} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </SettingsGroup>
      <SettingsGroup>
        <SettingRow label={t("usage.budget")} description={t("usage.budgetHint")}>
          <Input type="number" min={0} value={budget} onChange={(e) => setBudget(e.target.value)} placeholder={t("usage.noBudget")} aria-label={t("usage.budget")} style={{ width: 120 }} />
          <Button size="sm" onClick={() => void app.client.usage.budget.set({ monthly_usd: budget ? Number(budget) : null }).then(() => app.toast({ title: t("settings.saved"), tone: "success" }), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>
            {t("common.save")}
          </Button>
        </SettingRow>
      </SettingsGroup>
    </>
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
    <SettingsGroup title={t("settings.group.data")}>
      <SettingRow label={t("privacy.exportAll")} description={t("privacy.exportHint")}>
        <Button size="sm" disabled={busy} onClick={() => void exportAll()}>
          {t("privacy.export")}
        </Button>
      </SettingRow>
      <DeleteAllData />
    </SettingsGroup>
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
    <>
      <SettingsGroup title={t("developer.daemon")}>
        {status.error ? <Unavailable error={status.error} /> : null}
        {status.data && (
          <SettingRow
            label={`v${status.data.version} · ${status.data.channel} · pid ${status.data.pid} · up ${Math.round(status.data.uptime_s / 60)} min · ${status.data.sessions_running} running`}
            description={
              <>
                <div>{status.data.socket}</div>
                <div>{status.data.clients.map((c) => `${c.client_kind} ${c.client_version}`).join(", ")}</div>
              </>
            }
          />
        )}
        <div className="row wrap">
          <Button size="sm" icon="refresh" onClick={() => void app.client.daemon.restart({ stop_running: false }).catch(fail)}>
            {t("developer.restart")}
          </Button>
          <Button size="sm" onClick={() => void app.client.doctor.run({}).then((r) => setDoctor(r.checks), fail)}>
            {t("developer.doctor")}
          </Button>
          <Button size="sm" icon="copy" onClick={() => void app.client.diagnostics.collect({ include_logs: true }).then((r) => navigator.clipboard?.writeText(r.text)).then(() => app.toast({ title: t("chat.copied"), tone: "success" }), fail)}>
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
      </SettingsGroup>
      <SettingsGroup title={t("developer.problems")}>
        {problems.data?.problems.length === 0 && <p className="muted">{t("developer.noProblems")}</p>}
        {!!problems.data?.problems.length && (
          <div className="stack">
            {problems.data.problems.map((p, i) => (
              <Banner key={i} tone={p.severity === "error" ? "danger" : p.severity === "warning" ? "warning" : "info"}>
                {p.key && <code>{p.key}</code>} {p.message} {p.source && <span className="muted">({p.source})</span>}
              </Banner>
            ))}
          </div>
        )}
        {!problems.data && <p className="muted">{problems.error ? app.describeError(problems.error) : t("common.loading")}</p>}
      </SettingsGroup>
      <SettingsGroup title={t("developer.policy")}>{policy.data && Object.keys(policy.data.values ?? {}).length > 0 ? <pre className="code">{JSON.stringify(policy.data.values, null, 2)}</pre> : <p className="muted">{t("developer.noPolicy")}</p>}</SettingsGroup>
    </>
  );
}

export function AccountSection() {
  const app = useApp();
  const notices = useAsync(() => app.platform.notices(), []);
  const [name, setName] = useState(app.ui.displayName);
  return (
    <>
      <SettingsGroup title={t("account.profile")}>
        <SettingRow label={t("account.callYou")} description={t("account.callYouHint")} htmlFor="display-name">
          <Input id="display-name" value={name} placeholder={app.info?.user_name || t("sidebar.you")} onChange={(e) => setName(e.target.value)} onBlur={() => app.setUi({ displayName: name.trim() })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title={t("account.about")}>
        <SettingRow label={t("account.appVersion")}>
          <span className="muted">{app.info?.app_version ?? "—"}</span>
        </SettingRow>
        <SettingRow label={t("account.engineVersion")}>
          <span className="muted">{app.status.engine_version ?? "—"}</span>
        </SettingRow>
        <SettingRow label={t("account.licenceLabel")}>
          <span className="muted">{t("account.licence")}</span>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title={t("account.notices")}>
        {notices.loading ? (
          <p className="muted">{t("common.loading")}</p>
        ) : notices.data ? (
          <details className="notices">
            <summary>{t("account.showNotices")}</summary>
            <pre className="code notices__text">{notices.data}</pre>
          </details>
        ) : (
          <p className="muted">{t("account.noNotices")}</p>
        )}
      </SettingsGroup>
    </>
  );
}
