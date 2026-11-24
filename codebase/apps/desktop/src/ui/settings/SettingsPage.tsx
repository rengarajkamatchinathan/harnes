// Settings (05 §4, D34): a full-window page. Left column: Back, search over all settings, the
// section list and the profile row (Account & licence). Right: a centred column of titled
// cards. Plain values are generated from `config.schema` with live apply, inline
// "Saved"/validation errors, restart banner, admin locks and the scope picker (Code only);
// rich sections are hand-built.
import { useEffect, useMemo, useState } from "react";
import type { ConfigGetResult, ConfigScope } from "@genie/protocol";
import type { ReactNode } from "react";
import { Banner, Button, Icon, IconButton, Input, Segmented, Select, SettingRow, SettingsGroup, Switch, Textarea, type IconName } from "@genie/ui";
import { t, type MsgKey } from "../../i18n";
import { useApp, useAsync } from "../context";
import { initials } from "../Sidebar";
import { coerce, controlFor, description, keySchema, matches, NAV_SECTIONS, sectionOf, SECTIONS, uiKeys, type Control, type KeyMeta, type SectionId } from "./schema";
import { AccountSection, AppearanceSection, CapabilitiesSection, ConnectorsSection, DeveloperSection, GeneralSection, GenieSection, PrivacySection, UsageSection } from "./Sections";
import { ProvidersSection, providerMeta } from "./ProvidersSection";
import { ModelsSection } from "./ModelsSection";
import { PermissionsSection } from "./ManageSections";

const sectionName = (s: SectionId) => t(`settings.section.${s}` as MsgKey);

/** The card title over a section's schema-generated settings. */
const SCHEMA_GROUP: Partial<Record<SectionId, MsgKey>> = {
  general: "settings.group.updates",
  models: "settings.group.defaults",
  genie: "settings.group.memory",
  capabilities: "settings.group.capabilities",
  connectors: "settings.group.config",
  permissions: "settings.group.permissions",
  privacy: "settings.group.telemetry",
  developer: "settings.group.engine",
};

/** A line under the page title, where the reference has one. */
const LEDE: Partial<Record<SectionId, MsgKey>> = {
  models: "settings.lede.models",
  providers: "providers.lede",
};

/** Hand-built settings, so search finds them too (schema keys are searched directly). */
const HAND_BUILT: { section: SectionId; label: MsgKey; hint?: MsgKey }[] = [
  { section: "general", label: "settings.osNotifications", hint: "settings.osNotificationsHint" },
  { section: "general", label: "settings.completionSound", hint: "settings.completionSoundHint" },
  { section: "general", label: "settings.import", hint: "settings.importHint" },
  { section: "general", label: "settings.installCli", hint: "settings.installCliHint" },
  { section: "appearance", label: "appearance.theme", hint: "appearance.themeHint" },
  { section: "appearance", label: "appearance.density", hint: "appearance.densityHint" },
  { section: "appearance", label: "appearance.wrap", hint: "appearance.wrapHint" },
  { section: "appearance", label: "appearance.zoom", hint: "appearance.zoomHint" },
  { section: "appearance", label: "appearance.fontSize", hint: "appearance.fontSizeHint" },
  { section: "appearance", label: "appearance.chatFont", hint: "appearance.chatFontHint" },
  { section: "genie", label: "memory.saved" },
  { section: "models", label: "models.list", hint: "settings.lede.models" },
  { section: "capabilities", label: "capabilities.skills" },
  { section: "capabilities", label: "capabilities.browser" },
  { section: "connectors", label: "connectors.title" },
  { section: "permissions", label: "permissions.rules" },
  { section: "permissions", label: "permissions.trusted" },
  { section: "usage", label: "usage.budget", hint: "usage.budgetHint" },
  { section: "privacy", label: "privacy.exportAll", hint: "privacy.exportHint" },
  { section: "privacy", label: "privacy.deleteAll", hint: "privacy.deleteAllHint" },
  { section: "developer", label: "developer.daemon" },
  { section: "developer", label: "developer.copyDiagnostics" },
  { section: "account", label: "account.callYou", hint: "account.callYouHint" },
  { section: "account", label: "account.licenceLabel" },
];

export function SettingsPage() {
  const app = useApp();
  const section = (SECTIONS.includes(app.ui.settingsSection as SectionId) ? app.ui.settingsSection : "general") as SectionId;
  const [q, setQ] = useState("");
  const schema = useAsync(() => app.client.config.schema({}), []);
  const policy = useAsync(() => app.client.policy.effective({}), []);
  const org = (policy.data?.values?.["org.name"] as string | undefined) ?? "your organisation";
  const keys = useMemo(() => uiKeys(schema.data?.ui), [schema.data]);
  const [restart, setRestart] = useState(false);

  // Hand-built panels draw these keys themselves: Appearance its theme control, Models the
  // per-model switches, and Providers tucks the raw `providers` JSON under its Advanced disclosure.
  const own = (k: KeyMeta) => k.key !== "theme" && k.key !== "providers" && k.key !== "hiddenModels";
  const rawProviders = keys.filter((k) => k.key === "providers");
  const shown = q ? keys.filter((k) => own(k) && matches(q, k, keySchema(schema.data?.schema, k.key))) : keys.filter((k) => own(k) && sectionOf(k.key) === section);
  const needle = q.trim().toLowerCase();
  const handHits = needle ? HAND_BUILT.filter((h) => [t(h.label), h.hint ? t(h.hint) : "", sectionName(h.section)].some((x) => x.toLowerCase().includes(needle))) : [];

  const go = (s: SectionId) => (setQ(""), app.setUi({ settingsSection: s, settingsItem: null }));
  const close = () => app.setUi({ view: app.settingsFrom === "settings" ? "main" : app.settingsFrom });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !e.defaultPrevented && !document.querySelector(".g-dialog, [data-radix-popper-content-wrapper]") && close();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const schemaFields = (metas: KeyMeta[], title?: string) => (metas.length ? <SchemaFields metas={metas} title={title} schema={schema.data?.schema} org={org} onRestart={() => setRestart(true)} /> : null);
  const groupTitle = SCHEMA_GROUP[section] ? t(SCHEMA_GROUP[section]!) : undefined;
  const fields = schemaFields(shown, groupTitle);

  const body: Record<SectionId, () => ReactNode> = {
    general: () => <GeneralSection updates={fields} onAccount={() => go("account")} />,
    appearance: () => <AppearanceSection />,
    genie: () => (
      <>
        {fields}
        <GenieSection />
      </>
    ),
    models: () => (
      <>
        {fields}
        <ModelsSection />
      </>
    ),
    providers: () => <ProvidersSection advanced={rawProviders.length ? <SchemaFields metas={rawProviders} schema={schema.data?.schema} org={org} onRestart={() => setRestart(true)} bare /> : undefined} />,
    capabilities: () => (
      <>
        {fields}
        <CapabilitiesSection />
      </>
    ),
    connectors: () => (
      <>
        <ConnectorsSection />
        {fields}
      </>
    ),
    permissions: () => (
      <>
        <PermissionsSection />
        {fields}
      </>
    ),
    usage: () => <UsageSection />,
    privacy: () => (
      <>
        {fields}
        <PrivacySection />
      </>
    ),
    developer: () => (
      <>
        <DeveloperSection />
        {fields}
      </>
    ),
    account: () => <AccountSection />,
  };

  // Search: schema keys grouped by section, then hand-built settings that jump to their section.
  const searchResults = () => {
    const bySection = SECTIONS.map((s) => [s, shown.filter((k) => sectionOf(k.key) === s)] as const).filter(([, ks]) => ks.length);
    return (
      <>
        {bySection.map(([s, ks]) => (
          <SchemaFields key={s} metas={ks} title={sectionName(s)} schema={schema.data?.schema} org={org} onRestart={() => setRestart(true)} />
        ))}
        {handHits.length > 0 && (
          <SettingsGroup title={t("settings.search.more")}>
            {handHits.map((h) => (
              <SettingRow key={`${h.section}/${h.label}`} label={t(h.label)} description={h.hint ? `${sectionName(h.section)} · ${t(h.hint)}` : sectionName(h.section)}>
                <Button size="sm" onClick={() => go(h.section)}>
                  {t("settings.search.open")}
                </Button>
              </SettingRow>
            ))}
          </SettingsGroup>
        )}
        {!bySection.length && !handHits.length && <p className="muted settings-empty">{t("settings.noMatch", { q })}</p>}
      </>
    );
  };

  const lede = !q && LEDE[section];
  return (
    <div className="settings" role="region" aria-label={t("settings.title")}>
      <aside className="settings__side">
        <div className="settings__drag" data-tauri-drag-region />
        <button type="button" className="settings__back" onClick={close}>
          <Icon name="arrowLeft" size={15} />
          {t("settings.back")}
        </button>
        <label className="search settings__search">
          <Icon name="search" size={14} />
          <span className="g-sr-only">{t("settings.search")}</span>
          <input value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === "Escape" && q && (e.preventDefault(), setQ(""))} placeholder={t("settings.search")} />
        </label>
        <nav className="settings__nav" aria-label={t("settings.title")}>
          {NAV_SECTIONS.map((s) => (
            <div key={s} className="settings__navgroup">
              {NAV_BREAK.has(s) && <hr className="settings__sep" />}
              <button type="button" className={`settings__navitem ${!q && s === section ? "is-active" : ""}`} aria-current={!q && s === section ? "page" : undefined} onClick={() => go(s)}>
                <Icon name={SECTION_ICON[s]} size={15} />
                {sectionName(s)}
              </button>
            </div>
          ))}
        </nav>
        <div className="settings__foot">
          <button type="button" className={`settings__profile ${!q && section === "account" ? "is-active" : ""}`} aria-current={!q && section === "account" ? "page" : undefined} onClick={() => go("account")}>
            <span className="avatar" aria-hidden>
              {initials(app.userName)}
            </span>
            <span className="settings__profile-name">{app.userName || t("sidebar.you")}</span>
          </button>
          <IconButton icon="settings" size="sm" label={t("settings.profileSettings")} onClick={() => go("account")} />
        </div>
      </aside>
      <div className="settings__body">
        <div className="settings__drag settings__drag--body" data-tauri-drag-region />
        <div className="settings__inner">
          <header className="settings__header">
            <h1 className="settings__title">{q ? t("settings.search") : sectionName(section)}</h1>
            {lede && <p className="settings__lede">{t(lede)}</p>}
          </header>
          {restart && <Banner tone="warning">{t("settings.restart")}</Banner>}
          {schema.error ? <Banner tone="danger">{app.describeError(schema.error)}</Banner> : null}
          <div className="settings__content">{q ? searchResults() : body[section]()}</div>
        </div>
      </div>
    </div>
  );
}

export const SECTION_ICON: Record<SectionId, IconName> = {
  general: "settings",
  appearance: "palette",
  genie: "memory",
  models: "model",
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
const NAV_BREAK = new Set<SectionId>(["usage"]);

const MODEL_KEYS = new Set(["model", "smallModel"]);

/** "accept_edits" → "Accept edits" for enum options the schema leaves raw. */
export function humanize(v: string): string {
  const s = v.replace(/[_-]+/g, " ").trim();
  return s ? s[0].toUpperCase() + s.slice(1) : v;
}

function SchemaFields({ metas, title, schema, org, onRestart, bare }: { metas: KeyMeta[]; title?: string; schema: unknown; org: string; onRestart: () => void; /** No schema descriptions (the caller explains). */ bare?: boolean }) {
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
      bare={bare}
    />
  );
  return (
    <SettingsGroup
      title={title}
      action={scopable ? <Segmented label={t("settings.scope")} value={scope} onChange={(v) => setScope(v)} options={(["user", "project", "local"] as const).map((s) => ({ value: s, label: t(`settings.scope.${s}`) }))} /> : undefined}
    >
      {basic.map(render)}
      {advanced.length > 0 && (
        <details className="g-sgroup__flush settings-adv">
          <summary>
            <Icon name="chevronRight" size={14} />
            {t("settings.advanced")}
          </summary>
          <div className="settings-adv__rows">{advanced.map(render)}</div>
        </details>
      )}
    </SettingsGroup>
  );
}

function SchemaField({ meta, schema, value, source, locked, scope, cwd, onRestart, bare }: { meta: KeyMeta; schema: Record<string, unknown> | undefined; value: unknown; source?: string; locked: string | null; scope: ConfigScope; cwd: string | null; onRestart: () => void; bare?: boolean }) {
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
          options={[...(draft == null || draft === "" ? [{ value: "", label: t("settings.default"), disabled: true }] : []), ...control.options.map((o) => ({ value: o, label: humanize(o) }))]}
        />
      );
      break;
    case "number":
      input = <Input id={id} type="number" disabled={!!locked} value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => void commit(draft)} style={{ width: 120 }} />;
      break;
    case "list":
    case "json":
      input = <Textarea id={id} disabled={!!locked} className="mono" value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => void commit(draft)} rows={control.kind === "json" ? 6 : 3} />;
      break;
    default:
      // Model keys pick from the engine's model list instead of a free-text id.
      input = MODEL_KEYS.has(meta.key) && app.usableModels.length ? (
        <Select
          id={id}
          disabled={!!locked}
          value={String(draft ?? "")}
          onChange={(v) => (setDraft(v), void commit(v))}
          options={[
            ...(draft == null || draft === "" || !app.usableModels.some((m) => m.id === draft) ? [{ value: String(draft ?? ""), label: draft ? t("settings.modelUnavailable", { id: String(draft) }) : t("settings.default"), disabled: true }] : []),
            ...app.usableModels.map((m) => ({ value: m.id, label: m.name || m.id, group: providerMeta({ id: m.provider, kind: m.provider }).name })),
          ]}
        />
      ) : (
        <Input id={id} disabled={!!locked} value={String(draft ?? "")} onChange={(e) => setDraft(e.target.value)} onBlur={() => draft !== value && void commit(draft)} />
      );
  }
  return (
    <SettingRow
      label={meta.title}
      htmlFor={id}
      description={bare ? undefined : description(schema)}
      error={error}
      saved={saved}
      locked={locked}
      stacked={control.kind === "list" || control.kind === "json"}
      extra={source && source !== "default" && source !== "user" ? <span className="muted">({source})</span> : undefined}
    >
      {input}
    </SettingRow>
  );
}
