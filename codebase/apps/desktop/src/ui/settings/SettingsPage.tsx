// Settings (05 §4, D34): a large modal over the current view (Claude Desktop pattern); section nav, search over all
// settings, deep links; schema-generated controls from `config.schema` with live apply,
// inline "Saved"/validation errors, restart banner, admin locks, scope picker (Code only).
import { Fragment, useEffect, useMemo, useState } from "react";
import type { ConfigGetResult, ConfigScope } from "@genie/protocol";
import type { ReactNode } from "react";
import { Banner, Field, Icon, IconButton, Input, Segmented, Select, Switch, Textarea, type IconName } from "@genie/ui";
import { t } from "../../i18n";
import { useApp, useAsync } from "../context";
import { coerce, controlFor, description, keySchema, matches, sectionOf, SECTIONS, uiKeys, type Control, type KeyMeta, type SectionId } from "./schema";
import { AccountSection, AppearanceSection, CapabilitiesSection, ConnectorsSection, DeveloperSection, GenieSection, GeneralExtras, PrivacySection, UsageSection } from "./Sections";
import { ProvidersSection } from "./ProvidersSection";
import { PermissionsSection } from "./ManageSections";

export function SettingsPage() {
  const app = useApp();
  const section = (SECTIONS.includes(app.ui.settingsSection as SectionId) ? app.ui.settingsSection : "general") as SectionId;
  const [q, setQ] = useState("");
  const schema = useAsync(() => app.client.config.schema({}), []);
  const policy = useAsync(() => app.client.policy.effective({}), []);
  const org = (policy.data?.values?.["org.name"] as string | undefined) ?? "your organisation";
  const keys = useMemo(() => uiKeys(schema.data?.ui), [schema.data]);
  const [restart, setRestart] = useState(false);

  // Appearance draws its own theme control; don't render the schema's copy beside it.
  const own = (k: KeyMeta) => k.key !== "theme";
  const shown = (q ? keys.filter((k) => matches(q, k, keySchema(schema.data?.schema, k.key))) : keys.filter((k) => sectionOf(k.key) === section)).filter((k) => q || own(k));

  const close = () => app.setUi({ view: "main" });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !document.querySelector(".g-dialog") && close();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="g-dialog__backdrop settings-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
    <div className="settings" role="dialog" aria-modal="true" aria-labelledby="settings-title">
      <div className="settings__head">
        <h2 id="settings-title">{t("settings.title")}</h2>
        <IconButton icon="x" size="sm" label={t("common.close")} onClick={close} />
      </div>
      <nav className="settings__nav" aria-label={t("settings.title")}>
        <label className="search">
          <Icon name="search" />
          <span className="g-sr-only">{t("settings.search")}</span>
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("settings.search")} />
        </label>
        {SECTIONS.map((s) => (
          <Fragment key={s}>
            {NAV_BREAK.has(s) && <hr className="settings__sep" />}
            <button className={`settings__navitem ${!q && s === section ? "is-active" : ""}`} aria-current={!q && s === section ? "page" : undefined} onClick={() => (setQ(""), app.setUi({ settingsSection: s, settingsItem: null }))}>
              <Icon name={SECTION_ICON[s]} />
              {t(`settings.section.${s}` as Parameters<typeof t>[0])}
            </button>
          </Fragment>
        ))}
      </nav>
      <div className="settings__body">
        <div className="page__inner">
          <h1 className="settings__title">{q ? t("settings.search") : t(`settings.section.${section}` as Parameters<typeof t>[0])}</h1>
          {restart && <Banner tone="warning">{t("settings.restart")}</Banner>}
          {schema.error ? <Banner tone="danger">{app.describeError(schema.error)}</Banner> : null}
          {section === "appearance" && !q && <AppearanceSection />}
          {shown.length > 0 && <SchemaFields metas={shown} schema={schema.data?.schema} org={org} onRestart={() => setRestart(true)} />}
          {q && !shown.length && <p className="muted">{t("settings.noMatch", { q })}</p>}
          {!q && (
            <>
              {section === "general" && <GeneralExtras />}
              {section === "genie" && <GenieSection />}
              {section === "providers" && <ProvidersSection />}
              {section === "capabilities" && <CapabilitiesSection />}
              {section === "connectors" && <ConnectorsSection />}
              {section === "permissions" && <PermissionsSection />}
              {section === "usage" && <UsageSection />}
              {section === "privacy" && <PrivacySection />}
              {section === "developer" && <DeveloperSection />}
              {section === "account" && <AccountSection />}
            </>
          )}
        </div>
      </div>
    </div>
    </div>
  );
}

const SECTION_ICON: Record<SectionId, IconName> = {
  general: "settings",
  appearance: "palette",
  genie: "memory",
  providers: "key",
  capabilities: "customize",
  connectors: "plug",
  permissions: "shield",
  usage: "chart",
  privacy: "lock",
  developer: "code",
  account: "user",
};
/** A divider above these, grouping the nav like the reference. */
const NAV_BREAK = new Set<SectionId>(["providers", "usage", "account"]);

const MODEL_KEYS = new Set(["model", "smallModel"]);

/** "accept_edits" → "Accept edits" for enum options the schema leaves raw. */
export function humanize(v: string): string {
  const s = v.replace(/[_-]+/g, " ").trim();
  return s ? s[0].toUpperCase() + s.slice(1) : v;
}

function SchemaFields({ metas, schema, org, onRestart }: { metas: KeyMeta[]; schema: unknown; org: string; onRestart: () => void }) {
  const app = useApp();
  const session = app.ui.activeSession ? app.sessions.get(app.ui.activeSession) : null;
  const cwd = app.ui.mode === "code" ? (session?.workspace_id ?? null) : null;
  const [scope, setScope] = useState<ConfigScope>("user");
  const [values, setValues] = useState<ConfigGetResult | null>(null);
  const keyList = metas.map((m) => m.key).join(",");
  useEffect(() => {
    const load = () =>
      app.client.config
        .get({ keys: keyList.split(","), cwd, scope: null })
        .then(setValues)
        .catch(() => setValues({ values: {}, sources: {}, locked: [] }));
    void load();
    return app.onConfigChanged(() => void load());
  }, [app, keyList, cwd]);
  const scopable = !!cwd && metas.some((m) => m.scopes.length > 1);
  const basic = metas.filter((m) => !m.advanced);
  const advanced = metas.filter((m) => m.advanced);
  const render = (m: KeyMeta) => (
    <SchemaField
      key={m.key}
      meta={m}
      schema={keySchema(schema, m.key)}
      value={values?.values[m.key]}
      source={values?.sources[m.key]}
      locked={values?.locked.includes(m.key) ? t("settings.managedBy", { org }) : null}
      scope={m.scopes.includes(scope) ? scope : "user"}
      cwd={cwd}
      onRestart={onRestart}
    />
  );
  return (
    <div className="settings-group">
      {scopable && (
        <div className="row">
          <span>{t("settings.scope")}</span>
          <Segmented label={t("settings.scope")} value={scope} onChange={(v) => setScope(v)} options={(["user", "project", "local"] as const).map((s) => ({ value: s, label: t(`settings.scope.${s}`) }))} />
        </div>
      )}
      {basic.map(render)}
      {advanced.length > 0 && (
        <details>
          <summary>{t("settings.advanced")}</summary>
          {advanced.map(render)}
        </details>
      )}
    </div>
  );
}

function SchemaField({ meta, schema, value, source, locked, scope, cwd, onRestart }: { meta: KeyMeta; schema: Record<string, unknown> | undefined; value: unknown; source?: string; locked: string | null; scope: ConfigScope; cwd: string | null; onRestart: () => void }) {
  const app = useApp();
  const control: Control = controlFor(schema);
  const [draft, setDraft] = useState<unknown>(value);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = `set-${meta.key}`;
  useEffect(() => setDraft(control.kind === "json" ? JSON.stringify(value ?? null, null, 2) : control.kind === "list" ? ((value as string[] | undefined) ?? []).join("\n") : value), [value, control.kind]);

  const commit = async (raw: unknown) => {
    setError(null);
    setSaved(false);
    try {
      const v = coerce(control, raw);
      await app.client.config.set({ key: meta.key, value: v, scope, cwd });
      setSaved(true);
      if (meta.restart) onRestart();
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : app.describeError(e));
    }
  };

  let input: ReactNode;
  switch (control.kind) {
    case "boolean":
      input = <Switch id={id} checked={!!draft} disabled={!!locked} onChange={(v) => (setDraft(v), void commit(v))} aria-label={meta.title} />;
      break;
    case "enum":
      input = (
        <Select
          id={id}
          disabled={!!locked}
          value={String(draft ?? "")}
          onChange={(v) => (setDraft(v), void commit(v))}
          options={[...(draft == null || draft === "" ? [{ value: "", label: t("settings.choose"), disabled: true }] : []), ...control.options.map((o) => ({ value: o, label: humanize(o) }))]}
        />
      );
      break;
    case "number":
      input = <Input id={id} type="number" disabled={!!locked} value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => void commit(draft)} />;
      break;
    case "list":
    case "json":
      input = <Textarea id={id} disabled={!!locked} className="mono" value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => void commit(draft)} rows={control.kind === "json" ? 6 : 3} />;
      break;
    default:
      // Model keys pick from the engine's model list instead of a free-text id.
      input = MODEL_KEYS.has(meta.key) && app.models.length ? (
        <Select
          id={id}
          disabled={!!locked}
          value={String(draft ?? "")}
          onChange={(v) => (setDraft(v), void commit(v))}
          options={[
            ...(draft == null || draft === "" || !app.models.some((m) => m.id === draft) ? [{ value: String(draft ?? ""), label: draft ? String(draft) : t("settings.choose"), disabled: true }] : []),
            ...app.models.map((m) => ({ value: m.id, label: m.name || m.id, group: m.provider })),
          ]}
        />
      ) : (
        <Input id={id} disabled={!!locked} value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => draft !== value && void commit(draft)} />
      );
  }
  return (
    <Field inline={control.kind !== "list" && control.kind !== "json"} label={meta.title} htmlFor={id} description={description(schema)} error={error} saved={saved} locked={locked} extra={source && source !== "default" && source !== "user" ? <span className="muted">({source})</span> : undefined}>
      {input}
    </Field>
  );
}

