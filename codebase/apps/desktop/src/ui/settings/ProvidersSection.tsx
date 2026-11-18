// Providers & Models (05 §4, 03 §10). Connected providers are collapsible rows (credentials,
// endpoint, Azure deployments, models, test); the rest are tiles that open a setup dialog
// (API key, Sign in with Microsoft via PKCE or device code, endpoint). The raw `providers`
// JSON sits under Advanced; the catalog status is a footnote.
import { useEffect, useState, type ReactNode } from "react";
import type { CredentialInfo, LoginStartResult, ModelInfo, ProviderInfo } from "@genie/protocol";
import { Banner, Button, Dialog, Field, Icon, IconButton, Input, Segmented, Spinner } from "@genie/ui";
import { plural, t, type MsgKey } from "../../i18n";
import { useApp, useAsync } from "../context";

// ---- display metadata -----------------------------------------------------------------

interface Meta {
  name: string;
  blurb: MsgKey;
  /** Where to create an API key (opened in the system browser). */
  keyUrl?: string;
  /** Needs a base URL before it works. */
  endpoint?: "required" | "optional";
  /** Offers Microsoft (Entra) sign-in. */
  microsoft?: boolean;
  /** Works without any credential. */
  keyless?: boolean;
  placeholder?: string;
}

const META: Record<string, Meta> = {
  anthropic: { name: "Anthropic", blurb: "providers.blurb.anthropic", keyUrl: "https://console.anthropic.com/settings/keys" },
  openai: { name: "OpenAI", blurb: "providers.blurb.openai", keyUrl: "https://platform.openai.com/api-keys" },
  google: { name: "Google Gemini", blurb: "providers.blurb.google", keyUrl: "https://aistudio.google.com/apikey" },
  "azure-foundry": { name: "Azure AI Foundry", blurb: "providers.blurb.azure", endpoint: "required", microsoft: true, placeholder: "https://<resource>.services.ai.azure.com" },
  openrouter: { name: "OpenRouter", blurb: "providers.blurb.openrouter", keyUrl: "https://openrouter.ai/keys" },
  ollama: { name: "Ollama", blurb: "providers.blurb.ollama", endpoint: "optional", keyless: true, placeholder: "http://localhost:11434" },
};

export function providerMeta(p: Pick<ProviderInfo, "id" | "kind">): Meta {
  if (META[p.id]) return META[p.id];
  if (META[p.kind]) return { ...META[p.kind], name: humanizeId(p.id) };
  if (/vertex/i.test(p.kind)) return { name: humanizeId(p.id), blurb: "providers.blurb.vertex" };
  return { name: humanizeId(p.id), blurb: "providers.blurb.custom", endpoint: "required", placeholder: "https://api.example.com/v1" };
}

function humanizeId(id: string): string {
  return id.replace(/[-_]+/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function host(url?: string | null): string {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * Connected = the user set it up: it has a credential (stored or from the environment), it is
 * a custom endpoint they added, or it's keyless (Ollama) and found models on this computer.
 * Not "has models": the catalog lists every provider's models whether connected or not.
 */
function isConnected(p: ProviderInfo, models: ModelInfo[]): boolean {
  if (p.status === "disabled") return false;
  if (p.credentials.length > 0) return true;
  if (!META[p.id] && !META[p.kind]) return true;
  return !!providerMeta(p).keyless && p.status === "ready" && models.length > 0;
}

const isEnv = (c: CredentialInfo) => c.credential_id.startsWith("env:");

function credText(c: CredentialInfo): string {
  if (isEnv(c)) return c.label;
  if (c.kind === "oauth") return c.hint ? t("providers.cred.oauthAs", { who: c.hint }) : t("providers.cred.oauth");
  if (c.kind === "external") return c.label || t("providers.cred.external");
  return c.hint ? `${t("providers.apiKey")} ••••${c.hint}` : t("providers.apiKey");
}

type ProvidersConfig = Record<string, Record<string, unknown>>;

// ---- section --------------------------------------------------------------------------

export function ProvidersSection({ advanced }: { advanced?: ReactNode }) {
  const app = useApp();
  const catalog = useAsync(() => app.client.catalog.status({}), []);
  const [cfg, setCfg] = useState<ProvidersConfig>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [connect, setConnect] = useState<ProviderInfo | "custom" | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    const load = () =>
      app.client.config
        .get({ keys: ["providers"], cwd: null, scope: null })
        .then((r) => setCfg((r.values.providers as ProvidersConfig | undefined) ?? {}))
        .catch(() => setCfg({}));
    void load();
    return app.onConfigChanged(() => void load());
  }, [app]);

  const modelsOf = (id: string) => app.models.filter((m) => m.provider === id);
  const connected = app.providers.filter((p) => isConnected(p, modelsOf(p.id)));
  const available = app.providers.filter((p) => !isConnected(p, modelsOf(p.id)));
  const toggle = (id: string) =>
    setOpen((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const refreshCatalog = async () => {
    setRefreshing(true);
    try {
      await app.client.catalog.refresh({});
      catalog.reload();
      await app.refreshLists();
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="providers">
      <p className="muted providers__lede">{t("providers.lede")}</p>

      <h3 className="section-title">{t("providers.connected")}</h3>
      {connected.length ? (
        <div className="prov-list">
          {connected.map((p) => (
            <ProviderRow key={p.id} p={p} models={modelsOf(p.id)} cfg={cfg[p.id] ?? {}} open={open.has(p.id)} onToggle={() => toggle(p.id)} />
          ))}
        </div>
      ) : (
        <div className="prov-empty">
          <Icon name="plug" />
          <span>{t("providers.noneConnected")}</span>
        </div>
      )}

      <h3 className="section-title">{t("providers.add")}</h3>
      <div className="prov-grid">
        {available.map((p) => (
          <ProviderTile key={p.id} p={p} onConnect={() => setConnect(p)} />
        ))}
        <button type="button" className="prov-tile prov-tile--custom" onClick={() => setConnect("custom")}>
          <span className="prov-mark prov-mark--ghost" aria-hidden>
            <Icon name="plus" />
          </span>
          <span className="prov-tile__text">
            <span className="prov-tile__name">{t("providers.custom")}</span>
            <span className="prov-tile__blurb">{t("providers.customBlurb")}</span>
          </span>
        </button>
      </div>

      <div className="prov-foot">
        <span className="muted">
          {catalog.data ? catalogText(catalog.data.source, catalog.data.fetched_at) : t("providers.catalog.loading")}
        </span>
        <Button size="sm" variant="ghost" icon="refresh" disabled={refreshing} onClick={() => void refreshCatalog()}>
          {refreshing ? t("providers.refreshing") : t("providers.refresh")}
        </Button>
      </div>

      {advanced && (
        <details className="prov-advanced">
          <summary>
            <Icon name="chevronRight" size={14} />
            {t("providers.advanced")}
          </summary>
          <p className="muted settings-note">{t("providers.advancedHint")}</p>
          {advanced}
        </details>
      )}

      <Dialog open={!!connect} width={480} title={connect === "custom" ? t("providers.custom") : connect ? t("providers.connectTitle", { name: providerMeta(connect).name }) : ""} onClose={() => setConnect(null)}>
        {connect === "custom" ? (
          <CustomForm cfg={cfg} onDone={(id) => (setConnect(null), id && setOpen(new Set([id])))} />
        ) : connect ? (
          <ConnectForm p={connect} cfg={cfg[connect.id] ?? {}} onDone={(ok) => (setConnect(null), ok && setOpen(new Set([connect.id])))} onCancel={() => setConnect(null)} />
        ) : null}
      </Dialog>
    </div>
  );
}

function catalogText(source: string, fetchedAt: string): string {
  const when = fetchedAt ? new Date(fetchedAt) : null;
  if (source === "bundled" || !when || Number.isNaN(when.getTime())) return t("providers.catalog.bundled");
  return t("providers.catalog.updated", { when: when.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) });
}

// ---- pieces ---------------------------------------------------------------------------

function Mark({ name, ghost }: { name: string; ghost?: boolean }) {
  const letters = name
    .split(/\s+/)
    .slice(0, 2)
    .map((w) => w[0])
    .join("");
  return (
    <span className={`prov-mark ${ghost ? "prov-mark--ghost" : ""}`} aria-hidden>
      {letters}
    </span>
  );
}

function Status({ p }: { p: ProviderInfo }) {
  const tone = p.status === "ready" ? "ok" : p.status === "needs_auth" ? "warn" : p.status === "disabled" ? "off" : "bad";
  const label = t(`providers.status.${p.status === "unknown" ? "error" : p.status}` as MsgKey);
  return (
    <span className={`prov-status prov-status--${tone}`}>
      <span className="prov-status__dot" aria-hidden />
      {label}
    </span>
  );
}

function ProviderTile({ p, onConnect }: { p: ProviderInfo; onConnect: () => void }) {
  const meta = providerMeta(p);
  const blocked = p.status === "disabled";
  return (
    <div className={`prov-tile ${blocked ? "is-blocked" : ""}`}>
      <Mark name={meta.name} ghost={blocked} />
      <span className="prov-tile__text">
        <span className="prov-tile__name">{meta.name}</span>
        <span className="prov-tile__blurb">
          {blocked ? (
            <>
              <Icon name="lock" size={12} /> {t("providers.blocked")}
            </>
          ) : (
            t(meta.blurb)
          )}
        </span>
      </span>
      {!blocked && (
        <Button size="sm" onClick={onConnect}>
          {t("providers.connect")}
        </Button>
      )}
    </div>
  );
}

function ProviderRow({ p, models, cfg, open, onToggle }: { p: ProviderInfo; models: ModelInfo[]; cfg: Record<string, unknown>; open: boolean; onToggle: () => void }) {
  const app = useApp();
  const meta = providerMeta(p);
  const active = p.credentials.find((c) => c.active) ?? p.credentials[0];
  const sub = [active ? credText(active) : meta.keyless ? t("providers.cred.none") : null, models.length ? plural(models.length, { one: "# model", other: "# models" }) : null, meta.endpoint && p.endpoint ? host(p.endpoint) : null].filter(Boolean).join(" · ");
  const bodyId = `prov-${p.id}`;
  return (
    <div className={`prov-row ${open ? "is-open" : ""} ${p.status === "error" ? "is-error" : ""}`}>
      <button type="button" className="prov-row__head" aria-expanded={open} aria-controls={bodyId} onClick={onToggle}>
        <Mark name={meta.name} />
        <span className="prov-row__text">
          <span className="prov-row__name">{meta.name}</span>
          <span className="prov-row__sub">{sub}</span>
        </span>
        <Status p={p} />
        <span className="prov-row__chev" aria-hidden>
          <Icon name="chevronDown" size={14} />
        </span>
      </button>
      {open && (
        <div className="prov-row__body" id={bodyId}>
          {p.status === "error" && meta.endpoint === "required" && !cfg.base_url && <Banner tone="warning">{t("providers.needsEndpoint")}</Banner>}
          {!meta.keyless && <Credentials p={p} meta={meta} />}
          {meta.endpoint && <EndpointField p={p} meta={meta} cfg={cfg} />}
          {p.kind === "azure-foundry" && <Deployments p={p} cfg={cfg} />}
          <Models models={models} keyless={!!meta.keyless} />
          <TestBar p={p} onRemove={META[p.id] ? undefined : () => void removeCustom(app, p.id)} />
        </div>
      )}
    </div>
  );
}

async function removeCustom(app: ReturnType<typeof useApp>, id: string) {
  try {
    const r = await app.client.config.get({ keys: ["providers"], cwd: null, scope: "user" });
    const all = { ...((r.values.providers as ProvidersConfig | undefined) ?? {}) };
    delete all[id];
    await app.client.config.set({ key: "providers", value: all, scope: "user", cwd: null });
    await app.refreshLists();
  } catch (e) {
    app.toast({ title: app.describeError(e), tone: "danger" });
  }
}

function Sub({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="prov-sub">
      <div className="prov-sub__head">
        <h4>{title}</h4>
        {action}
      </div>
      {children}
    </section>
  );
}

function Credentials({ p, meta }: { p: ProviderInfo; meta: Meta }) {
  const app = useApp();
  const [adding, setAdding] = useState(false);
  const [confirm, setConfirm] = useState<string | null>(null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const refresh = () => app.refreshLists();
  useEffect(() => {
    if (!confirm) return;
    const h = setTimeout(() => setConfirm(null), 3000);
    return () => clearTimeout(h);
  }, [confirm]);

  return (
    <Sub
      title={t("providers.credentials")}
      action={
        !adding && (
          <Button size="sm" variant="ghost" icon="plus" onClick={() => setAdding(true)}>
            {meta.microsoft ? t("providers.addCredential") : t("providers.addAnotherKey")}
          </Button>
        )
      }
    >
      <ul className="plain prov-creds">
        {p.credentials.map((c) => (
          <li key={c.credential_id} className="prov-cred">
            <span className={`prov-radio ${c.active ? "is-on" : ""}`} aria-hidden />
            <span className="grow">
              <span className="prov-cred__label">{isEnv(c) ? t("providers.cred.env") : c.label}</span>
              <span className="muted"> · {c.kind === "key" ? (c.hint ? `••••${c.hint}` : t("providers.apiKey")) : credText(c)}</span>
            </span>
            {c.active ? (
              <span className="muted prov-cred__state">{t("providers.inUse")}</span>
            ) : (
              <Button size="sm" variant="ghost" onClick={() => void app.client.provider.credential.activate({ credential_id: c.credential_id }).then(refresh, fail)}>
                {t("providers.activate")}
              </Button>
            )}
            {!isEnv(c) &&
              (confirm === c.credential_id ? (
                <Button size="sm" variant="danger" onClick={() => void app.client.provider.credential.remove({ credential_id: c.credential_id }).then(refresh, fail)}>
                  {t("providers.confirmRemove")}
                </Button>
              ) : (
                <IconButton icon="trash" size="sm" label={t("providers.remove")} onClick={() => setConfirm(c.credential_id)} />
              ))}
          </li>
        ))}
      </ul>
      {adding && (
        <div className="prov-inline">
          <AuthForm p={p} meta={meta} onDone={() => setAdding(false)} onCancel={() => setAdding(false)} />
        </div>
      )}
    </Sub>
  );
}

function EndpointField({ p, meta, cfg }: { p: ProviderInfo; meta: Meta; cfg: Record<string, unknown> }) {
  const app = useApp();
  const current = (cfg.base_url as string | undefined) ?? "";
  const [v, setV] = useState(current);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setV(current), [current]);
  const save = async () => {
    setError(null);
    try {
      await saveProvider(app, p.id, { base_url: v.trim() || null });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(app.describeError(e));
    }
  };
  return (
    <Sub title={t("providers.endpoint")} action={saved && <span className="g-field__saved" role="status">{t("providers.saved")}</span>}>
      <div className="row">
        <Input id={`ep-${p.id}`} aria-label={t("providers.endpoint")} className="mono" value={v} placeholder={meta.placeholder ?? p.endpoint ?? ""} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => e.key === "Enter" && v !== current && void save()} />
        <Button disabled={v === current} onClick={() => void save()}>
          {t("common.save")}
        </Button>
      </div>
      {meta.endpoint === "optional" && <p className="muted prov-hint">{t("providers.endpointDefault", { url: meta.placeholder ?? "" })}</p>}
      {error && <div className="error-text" role="alert">{error}</div>}
    </Sub>
  );
}

interface Dep {
  name: string;
  model: string;
}

function Deployments({ p, cfg }: { p: ProviderInfo; cfg: Record<string, unknown> }) {
  const app = useApp();
  const deps = (Array.isArray(cfg.deployments) ? cfg.deployments : []) as Dep[];
  const [name, setName] = useState("");
  const [model, setModel] = useState("");
  const write = (next: Dep[]) => saveProvider(app, p.id, { deployments: next }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  const add = async () => {
    await write([...deps, { name: name.trim(), model: model.trim() || name.trim() }]);
    setName("");
    setModel("");
  };
  return (
    <Sub title={t("providers.deployments")}>
      <p className="muted prov-hint">{t("providers.deploymentsHint")}</p>
      {deps.length > 0 && (
        <ul className="plain prov-table">
          {deps.map((d, i) => (
            <li key={`${d.name}-${i}`}>
              <code className="grow">{d.name}</code>
              <span className="muted">{d.model}</span>
              <IconButton icon="x" size="sm" label={t("providers.remove")} onClick={() => void write(deps.filter((_, j) => j !== i))} />
            </li>
          ))}
        </ul>
      )}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          void add();
        }}
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("providers.depName")} aria-label={t("providers.depName")} />
        <Input value={model} onChange={(e) => setModel(e.target.value)} placeholder={t("providers.depModel")} aria-label={t("providers.depModel")} />
        <Button type="submit" icon="plus" disabled={!name.trim()}>
          {t("providers.add.short")}
        </Button>
      </form>
    </Sub>
  );
}

const MODELS_SHOWN = 6;

function Models({ models, keyless }: { models: ModelInfo[]; keyless: boolean }) {
  const [all, setAll] = useState(false);
  const shown = all ? models : models.slice(0, MODELS_SHOWN);
  return (
    <Sub title={t("providers.models")}>
      {!models.length ? (
        <p className="muted prov-hint">{keyless ? t("providers.noModelsLocal") : t("providers.noModels")}</p>
      ) : (
        <>
          <ul className="plain prov-models">
            {shown.map((m) => (
              <li key={m.id}>
                <span className="grow prov-model__name">
                  {m.name || m.id}
                  {m.name && m.name !== m.id && <code className="muted">{m.id.replace(`${m.provider}/`, "")}</code>}
                </span>
                {!!m.effort_levels?.length && <span className="prov-model__tag">{t("providers.effort")}</span>}
                {m.context_window ? <span className="muted prov-model__ctx">{t("providers.ctx", { k: Math.round(m.context_window / 1000) })}</span> : null}
              </li>
            ))}
          </ul>
          {models.length > MODELS_SHOWN && (
            <Button size="sm" variant="ghost" onClick={() => setAll(!all)}>
              {all ? t("providers.showFewer") : t("providers.showAll", { n: models.length })}
            </Button>
          )}
        </>
      )}
    </Sub>
  );
}

function TestBar({ p, onRemove }: { p: ProviderInfo; onRemove?: () => void }) {
  const [result, run, busy] = useTest(p.id);
  return (
    <div className="prov-testbar">
      <Button size="sm" icon="play" disabled={busy} onClick={() => void run()}>
        {t("providers.testConnection")}
      </Button>
      <TestResult result={result} busy={busy} />
      <span className="grow" />
      {onRemove && (
        <Button size="sm" variant="ghost" icon="trash" onClick={onRemove}>
          {t("providers.removeProvider")}
        </Button>
      )}
    </div>
  );
}

type TestState = { ok: boolean; text: string } | null;

function useTest(id: string): [TestState, () => Promise<boolean>, boolean] {
  const app = useApp();
  const [result, setResult] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    setResult(null);
    try {
      const r = await app.client.provider.test({ provider: id });
      setResult(r.ok ? { ok: true, text: t("providers.testOk", { ms: r.latency_ms }) } : { ok: false, text: r.error ?? t("providers.testFailed") });
      return r.ok;
    } catch (e) {
      setResult({ ok: false, text: app.describeError(e) });
      return false;
    } finally {
      setBusy(false);
    }
  };
  return [result, run, busy];
}

function TestResult({ result, busy }: { result: TestState; busy: boolean }) {
  if (busy)
    return (
      <span className="prov-test muted" role="status">
        <Spinner label={t("firstrun.testing")} /> {t("firstrun.testing")}
      </span>
    );
  if (!result) return null;
  return (
    <span className={`prov-test ${result.ok ? "is-ok" : "is-bad"}`} role="status">
      <Icon name={result.ok ? "check" : "warning"} size={14} />
      {result.text}
    </span>
  );
}

async function saveProvider(app: ReturnType<typeof useApp>, id: string, patch: Record<string, unknown>) {
  const r = await app.client.config.get({ keys: ["providers"], cwd: null, scope: "user" });
  const cur = ((r.values.providers as ProvidersConfig | undefined) ?? {})[id] ?? {};
  const next: Record<string, unknown> = { ...cur, ...patch };
  for (const k of Object.keys(next)) if (next[k] == null) delete next[k];
  await app.client.config.set({ key: `providers.${id}`, value: next, scope: "user", cwd: null });
  await app.refreshLists();
}

// ---- connect --------------------------------------------------------------------------

/**
 * Setup for one provider: endpoint when it needs one, then a key or Microsoft sign-in,
 * then a connection test. Used by the Connect dialog and by first run.
 */
export function ConnectForm({ p, cfg = {}, onDone, onCancel }: { p: ProviderInfo; cfg?: Record<string, unknown>; onDone: (ok: boolean) => void; onCancel?: () => void }) {
  const app = useApp();
  const meta = providerMeta(p);
  // First run has no settings to hand; the provider list carries a configured base URL.
  const base = (cfg.base_url as string | undefined) ?? (meta.endpoint === "required" ? (p.endpoint ?? "") : "");
  const [endpoint, setEndpoint] = useState(base);
  const [error, setError] = useState<string | null>(null);
  const [result, run, busy] = useTest(p.id);
  const needsEndpoint = meta.endpoint === "required" && !base;

  const saveEndpoint = async () => {
    const v = endpoint.trim();
    if (v === base) return true;
    try {
      await saveProvider(app, p.id, { base_url: v || null });
      return true;
    } catch (e) {
      setError(app.describeError(e));
      return false;
    }
  };

  const endpointField = meta.endpoint && (
    <Field label={t("providers.endpoint")} htmlFor={`cf-ep-${p.id}`} description={meta.endpoint === "optional" ? t("providers.endpointDefault", { url: meta.placeholder ?? "" }) : t("providers.endpointHint")}>
      <Input id={`cf-ep-${p.id}`} className="mono" value={endpoint} placeholder={meta.placeholder} onChange={(e) => setEndpoint(e.target.value)} autoFocus={meta.endpoint === "required"} />
    </Field>
  );

  if (meta.keyless) {
    return (
      <div className="prov-form">
        <p className="muted">{t(meta.blurb)}</p>
        {endpointField}
        {error && <Banner tone="danger">{error}</Banner>}
        <TestResult result={result} busy={busy} />
        <div className="g-dialog__footer">
          {onCancel && (
            <Button variant="ghost" onClick={onCancel}>
              {t("common.cancel")}
            </Button>
          )}
          <Button variant="primary" disabled={busy} onClick={() => void saveEndpoint().then((ok) => ok && run().then((r) => r && onDone(true)))}>
            {t("providers.connect")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="prov-form">
      {endpointField}
      <AuthForm
        p={p}
        meta={meta}
        autoFocus={!needsEndpoint}
        disabled={needsEndpoint && !endpoint.trim()}
        before={saveEndpoint}
        onDone={() => onDone(true)}
        onCancel={onCancel}
        submitLabel={t("providers.connect")}
        footer
      />
      {error && <Banner tone="danger">{error}</Banner>}
    </div>
  );
}

/** API key entry and/or Microsoft sign-in; tests the provider after a key is added. */
function AuthForm({
  p,
  meta,
  onDone,
  onCancel,
  before,
  disabled,
  autoFocus = true,
  submitLabel,
  footer,
}: {
  p: ProviderInfo;
  meta: Meta;
  onDone: () => void;
  onCancel?: () => void;
  before?: () => Promise<boolean>;
  disabled?: boolean;
  autoFocus?: boolean;
  submitLabel?: string;
  footer?: boolean;
}) {
  const app = useApp();
  const [method, setMethod] = useState<"microsoft" | "key">(meta.microsoft ? "microsoft" : "key");
  const [key, setKey] = useState("");
  const [label, setLabel] = useState("");
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<{ text: string; credential?: string } | null>(null);
  const [login, setLogin] = useState<LoginStartResult | null>(null);
  const [loginMsg, setLoginMsg] = useState<string | null>(null);
  const [result, run, busy] = useTest(p.id);

  // Poll a sign-in until it finishes.
  useEffect(() => {
    if (!login) return;
    const h = setInterval(async () => {
      try {
        const s = await app.client.provider.login.status({ login_id: login.login_id });
        if (s.state === "done") {
          setLogin(null);
          setLoginMsg(null);
          await app.refreshLists();
          onDone();
        } else if (s.state === "failed") {
          setLogin(null);
          setLoginMsg(s.error ?? t("providers.signInFailed"));
        }
      } catch (e) {
        setLogin(null);
        setLoginMsg(app.describeError(e));
      }
    }, 1500);
    return () => clearInterval(h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app, login]);

  const signIn = async (how: "pkce" | "device_code") => {
    setLoginMsg(null);
    if (before && !(await before())) return;
    try {
      const r = await app.client.provider.login.start({ provider: p.id, method: how });
      setLogin(r);
      if (r.auth_url) await app.platform.openExternal(r.auth_url);
    } catch (e) {
      setLoginMsg(app.describeError(e));
    }
  };

  const addKey = async () => {
    setProblem(null);
    setSaving(true);
    try {
      if (before && !(await before())) return;
      const r = await app.client.provider.credential.add({ provider: p.id, kind: "key", label: label.trim() || t("providers.apiKey"), secret: key.trim() });
      await app.refreshLists();
      if (await run()) {
        setKey("");
        setLabel("");
        app.toast({ title: t("providers.connectedToast", { name: meta.name }), tone: "success" });
        onDone();
      } else {
        setProblem({ text: t("providers.keySavedTestFailed"), credential: r.credential_id });
      }
    } catch (e) {
      setProblem({ text: app.describeError(e) });
    } finally {
      setSaving(false);
    }
  };

  const discard = async () => {
    if (problem?.credential) await app.client.provider.credential.remove({ credential_id: problem.credential }).catch(() => undefined);
    await app.refreshLists();
    setProblem(null);
  };

  const actions = (primary: ReactNode) =>
    footer ? (
      <div className="g-dialog__footer">
        {onCancel && (
          <Button variant="ghost" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
        )}
        {primary}
      </div>
    ) : (
      <div className="row end">
        {onCancel && (
          <Button variant="ghost" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
        )}
        {primary}
      </div>
    );

  return (
    <div className="prov-form">
      {meta.microsoft && (
        <Segmented
          label={t("providers.authMethod")}
          value={method}
          onChange={setMethod}
          options={[
            { value: "microsoft", label: t("providers.method.microsoft") },
            { value: "key", label: t("providers.method.key") },
          ]}
        />
      )}
      {method === "microsoft" ? (
        <>
          <p className="muted prov-hint">{t("providers.signInHint")}</p>
          {login ? (
            <div className="prov-login" role="status">
              <Spinner label={t("providers.waiting")} />
              <div className="grow">
                <div>{t("providers.waiting")}</div>
                {login.user_code && (
                  <div className="muted">
                    {t("providers.deviceCodeAt", { uri: login.verification_uri ?? "" })}
                    <code className="prov-code">{login.user_code}</code>
                  </div>
                )}
              </div>
              <Button size="sm" variant="ghost" onClick={() => setLogin(null)}>
                {t("common.cancel")}
              </Button>
            </div>
          ) : (
            actions(
              <>
                <Button variant="ghost" disabled={disabled} onClick={() => void signIn("device_code")}>
                  {t("providers.deviceCode")}
                </Button>
                <Button variant="primary" icon="external" disabled={disabled} onClick={() => void signIn("pkce")}>
                  {t("providers.signIn")}
                </Button>
              </>,
            )
          )}
          {loginMsg && <Banner tone="danger">{loginMsg}</Banner>}
        </>
      ) : (
        <form
          className="prov-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (key.trim() && !disabled) void addKey();
          }}
        >
          <Field
            label={t("providers.apiKey")}
            htmlFor={`key-${p.id}`}
            extra={
              meta.keyUrl ? (
                <button type="button" className="prov-link" onClick={() => void app.platform.openExternal(meta.keyUrl!)}>
                  {t("providers.getKey")} <Icon name="external" size={12} />
                </button>
              ) : undefined
            }
            description={t("providers.keyStored")}
          >
            <Input id={`key-${p.id}`} type="password" autoComplete="off" spellCheck={false} className="mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder={t("providers.keyPlaceholder")} autoFocus={autoFocus} />
          </Field>
          <Field label={t("providers.label")} htmlFor={`label-${p.id}`} extra={<span className="muted">{t("providers.optional")}</span>}>
            <Input id={`label-${p.id}`} value={label} onChange={(e) => setLabel(e.target.value)} placeholder={t("providers.labelPlaceholder")} />
          </Field>
          <TestResult result={result} busy={busy && !saving} />
          {problem && (
            <Banner
              tone="danger"
              actions={
                problem.credential ? (
                  <>
                    <Button size="sm" variant="ghost" onClick={() => void discard()}>
                      {t("providers.discardKey")}
                    </Button>
                    <Button size="sm" onClick={onDone}>
                      {t("providers.keepKey")}
                    </Button>
                  </>
                ) : undefined
              }
            >
              {problem.text}
              {result && !result.ok && <div className="muted">{result.text}</div>}
            </Banner>
          )}
          {!problem &&
            actions(
              <Button type="submit" variant="primary" disabled={!key.trim() || saving || disabled}>
                {saving ? t("providers.checking") : (submitLabel ?? t("providers.addKey"))}
              </Button>,
            )}
        </form>
      )}
    </div>
  );
}

/** An OpenAI-compatible endpoint (vLLM, LM Studio, a gateway…): id, base URL, optional key. */
function CustomForm({ cfg, onDone }: { cfg: ProvidersConfig; onDone: (id: string | null) => void }) {
  const app = useApp();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const id = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  const taken = !!id && (id in cfg || app.providers.some((p) => p.id === id));
  const validUrl = /^https?:\/\/\S+$/.test(url.trim());

  const save = async () => {
    setError(null);
    setSaving(true);
    try {
      await app.client.config.set({ key: `providers.${id}`, value: { kind: "openai-compatible", base_url: url.trim() }, scope: "user", cwd: null });
      await app.refreshLists();
      if (key.trim()) await app.client.provider.credential.add({ provider: id, kind: "key", label: t("providers.apiKey"), secret: key.trim() });
      await app.refreshLists();
      app.toast({ title: t("providers.connectedToast", { name: name.trim() }), tone: "success" });
      onDone(id);
    } catch (e) {
      setError(app.describeError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="prov-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (id && !taken && validUrl) void save();
      }}
    >
      <p className="muted prov-hint">{t("providers.customHint")}</p>
      <Field label={t("providers.customName")} htmlFor="custom-name" error={taken ? t("providers.customTaken") : null} description={id ? t("providers.customId", { id }) : undefined}>
        <Input id="custom-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="LM Studio" autoFocus />
      </Field>
      <Field label={t("providers.endpoint")} htmlFor="custom-url">
        <Input id="custom-url" className="mono" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://localhost:1234/v1" />
      </Field>
      <Field label={t("providers.apiKey")} htmlFor="custom-key" extra={<span className="muted">{t("providers.optional")}</span>}>
        <Input id="custom-key" type="password" autoComplete="off" className="mono" value={key} onChange={(e) => setKey(e.target.value)} placeholder={t("providers.keyPlaceholder")} />
      </Field>
      {error && <Banner tone="danger">{error}</Banner>}
      <div className="g-dialog__footer">
        <Button variant="ghost" onClick={() => onDone(null)}>
          {t("common.cancel")}
        </Button>
        <Button type="submit" variant="primary" disabled={!id || taken || !validUrl || saving}>
          {t("providers.addProvider")}
        </Button>
      </div>
    </form>
  );
}
