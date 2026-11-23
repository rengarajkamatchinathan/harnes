// Providers & Models (05 §4, 03 §10). Connected providers are collapsible rows (credentials,
// endpoint, Azure deployments, models, test); the rest are tiles that open a setup dialog.
// Setup asks for the least it can: one key for hosted APIs; endpoint + key + deployment for
// Azure (the portal's Target URI is accepted as-is and split into endpoint and deployment);
// nothing at all for a local Ollama. A follow-up question only appears when Genie can't
// infer the answer (e.g. which model an Azure deployment runs). The raw `providers` JSON sits
// under Advanced; the catalog status is a footnote.
import { useEffect, useMemo, useState, type CSSProperties, type ReactNode } from "react";
import type { CredentialInfo, LoginStartResult, ModelInfo, ProviderInfo } from "@genie/protocol";
import { Banner, Button, Dialog, Field, Icon, IconButton, Input, Spinner } from "@genie/ui";
import { plural, t, type MsgKey } from "../../i18n";
import { useApp, useAsync } from "../context";

type App = ReturnType<typeof useApp>;

// ---- display metadata -----------------------------------------------------------------

interface Meta {
  name: string;
  blurb: MsgKey;
  /** Short monogram and hue for the provider mark (no brand logos). */
  mono?: string;
  hue?: number;
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
  anthropic: { name: "Anthropic", blurb: "providers.blurb.anthropic", mono: "A", hue: 24, keyUrl: "https://console.anthropic.com/settings/keys" },
  openai: { name: "OpenAI", blurb: "providers.blurb.openai", mono: "O", hue: 160, keyUrl: "https://platform.openai.com/api-keys" },
  google: { name: "Google Gemini", blurb: "providers.blurb.google", mono: "G", hue: 220, keyUrl: "https://aistudio.google.com/apikey" },
  "azure-foundry": { name: "Azure AI Foundry", blurb: "providers.blurb.azure", mono: "Az", hue: 205, endpoint: "required", microsoft: true, placeholder: "https://my-resource.openai.azure.com" },
  openrouter: { name: "OpenRouter", blurb: "providers.blurb.openrouter", mono: "OR", hue: 265, keyUrl: "https://openrouter.ai/keys" },
  ollama: { name: "Ollama", blurb: "providers.blurb.ollama", mono: "Ol", hue: 40, endpoint: "optional", keyless: true, placeholder: "http://localhost:11434" },
};

export function providerMeta(p: Pick<ProviderInfo, "id" | "kind">): Meta {
  if (META[p.id]) return META[p.id];
  if (META[p.kind]) return { ...META[p.kind], name: humanizeId(p.id) };
  if (/vertex/i.test(p.kind)) return { name: humanizeId(p.id), blurb: "providers.blurb.vertex", hue: 220 };
  return { name: humanizeId(p.id), blurb: "providers.blurb.custom", endpoint: "required", placeholder: "https://api.example.com/v1" };
}

const isAzure = (p: Pick<ProviderInfo, "id" | "kind">) => p.kind === "azure-foundry" || p.id === "azure-foundry";

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

// ---- input clean-up -------------------------------------------------------------------

/** Keys are often pasted with quotes, a `Bearer ` prefix or a trailing newline. */
export function cleanKey(raw: string): string {
  return raw
    .trim()
    .replace(/^["'`]+|["'`]+$/g, "")
    .replace(/^Bearer\s+/i, "")
    .trim();
}

/** Accepts `host`, `https://host/` or a full URL and returns a normalized http(s) URL, or null. */
function parseUrl(raw: string): URL | null {
  const s = raw.trim();
  if (!s) return null;
  try {
    const u = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(s) ? s : `${/^(localhost|127\.|\[::1\])/i.test(s) ? "http" : "https"}://${s}`);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname || (!u.hostname.includes(".") && u.hostname !== "localhost" && !u.hostname.startsWith("["))) return null;
    return u;
  } catch {
    return null;
  }
}

/**
 * Azure: Genie calls `{base}/openai/v1/…` and `{base}/anthropic/v1/…`, so the base is the
 * resource root. People paste whatever the portal shows — the Target URI
 * (`…/openai/deployments/<name>/chat/completions?api-version=…`), the models inference URL,
 * or a project URL — so cut the API path off and keep the deployment name if there is one.
 * A gateway path in front of the API (APIM) is kept.
 */
export function parseAzureEndpoint(raw: string): { base: string; deployment?: string } | null {
  const u = parseUrl(raw);
  if (!u) return null;
  const dep = u.pathname.match(/\/openai\/deployments\/([^/?#]+)/i);
  const cut = u.pathname.search(/\/(openai|anthropic|models|api\/projects)(\/|$)/i);
  const path = (cut >= 0 ? u.pathname.slice(0, cut) : u.pathname).replace(/\/+$/, "");
  return { base: u.origin + path, deployment: dep ? decodeURIComponent(dep[1]) : undefined };
}

/** OpenAI-compatible: keep the path (it usually ends in /v1) but drop query and trailing slash. */
function normalizeBase(raw: string): string | null {
  const u = parseUrl(raw);
  return u ? u.origin + u.pathname.replace(/\/+$/, "") : null;
}

/** Turn a raw provider error into one plain sentence; the raw text is kept as detail. */
function explain(raw: string, ctx: { name: string; endpoint?: string | null; keyless?: boolean }): { text: string; detail?: string } {
  const where = host(ctx.endpoint) || ctx.name;
  if (/\b(401|403)\b|unauthori[sz]ed|forbidden|invalid[ _-]?(api[ _-]?)?key|incorrect api key|permission/i.test(raw)) return { text: t("providers.err.key"), detail: raw };
  if (/\b404\b|not found/i.test(raw)) return { text: t("providers.err.notFound", { where }), detail: raw };
  if (/dns|resolve|connect|refused|unreachable|timed? ?out|network|tls|certificate|error sending request/i.test(raw))
    return { text: ctx.keyless ? t("providers.err.local", { where }) : t("providers.err.unreachable", { where }), detail: raw };
  if (/\b429\b|rate limit|quota/i.test(raw)) return { text: t("providers.err.quota"), detail: raw };
  return { text: raw };
}

// ---- Azure deployments ----------------------------------------------------------------

interface Dep {
  name: string;
  model: string;
}

/** Wire ids of every catalog model outside Azure (what a deployment can run). */
function useKnownModels(): string[] {
  const app = useApp();
  return useMemo(() => {
    const azure = new Set(app.providers.filter(isAzure).map((p) => p.id));
    const ids = new Set<string>();
    for (const m of app.models) if (!azure.has(m.provider)) ids.add(m.id.startsWith(`${m.provider}/`) ? m.id.slice(m.provider.length + 1) : m.id);
    return [...ids].sort();
  }, [app.models, app.providers]);
}

/**
 * Which model a deployment runs. Deployments are usually named after their model
 * ("gpt-4o", "gpt-4o-mini-prod"), so an exact or prefix match answers it without asking.
 */
function matchModel(known: string[], name: string): { exact?: string; guess?: string } {
  const d = name.trim().toLowerCase();
  if (!d) return {};
  const exact = known.find((k) => k.toLowerCase() === d);
  if (exact) return { exact };
  const guess = known.filter((k) => d.startsWith(k.toLowerCase())).sort((a, b) => b.length - a.length)[0];
  return { guess };
}

/** Deployment name, plus the model only when it can't be inferred from the name. */
function DeploymentFields({ id, value, onChange, autoFocus }: { id: string; value: Dep; onChange: (d: Dep) => void; autoFocus?: boolean }) {
  const known = useKnownModels();
  const m = matchModel(known, value.name);
  const ask = !!value.name.trim() && !m.exact;
  const listId = `${id}-models`;
  return (
    <>
      <Field
        label={t("providers.deployment")}
        htmlFor={`${id}-name`}
        description={m.exact ? <span className="prov-ok"><Icon name="check" size={12} /> {t("providers.depRecognised", { model: m.exact })}</span> : t("providers.deploymentHint")}
      >
        <Input
          id={`${id}-name`}
          className="mono"
          value={value.name}
          spellCheck={false}
          autoComplete="off"
          placeholder="gpt-4o"
          autoFocus={autoFocus}
          onChange={(e) => {
            const name = e.target.value;
            const next = matchModel(known, name);
            // Keep a model the user typed; otherwise follow the best guess.
            const typed = value.model && value.model !== matchModel(known, value.name).guess;
            onChange({ name, model: typed ? value.model : (next.guess ?? "") });
          }}
        />
      </Field>
      {ask && (
        <Field label={t("providers.depModelQ")} htmlFor={`${id}-model`} extra={<span className="muted">{t("providers.optional")}</span>} description={t("providers.depModelHint")}>
          <Input id={`${id}-model`} className="mono" list={listId} value={value.model} spellCheck={false} autoComplete="off" placeholder={t("providers.depModelPlaceholder")} onChange={(e) => onChange({ ...value, model: e.target.value })} />
          <datalist id={listId}>
            {known.map((k) => (
              <option key={k} value={k} />
            ))}
          </datalist>
        </Field>
      )}
    </>
  );
}

/** The `{name, model}` saved for a deployment: the recognised model, else what was typed, else the name. */
function resolveDep(known: string[], d: Dep): Dep {
  const name = d.name.trim();
  return { name, model: matchModel(known, name).exact ?? (d.model.trim() || name) };
}

function withDep(deps: Dep[], d: Dep): Dep[] {
  return [...deps.filter((x) => x.name !== d.name), d];
}

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
  const done = (id: string | null) => {
    setConnect(null);
    if (id) setOpen(new Set([id]));
  };

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

  const meta = connect && connect !== "custom" ? providerMeta(connect) : null;

  return (
    <div className="providers">
      <p className="muted providers__lede">{t("providers.lede")}</p>

      {connected.length > 0 && (
        <>
          <h3 className="section-title">{t("providers.connected")}</h3>
          <div className="prov-list">
            {connected.map((p) => (
              <ProviderRow key={p.id} p={p} models={modelsOf(p.id)} cfg={cfg[p.id] ?? {}} open={open.has(p.id)} onToggle={() => toggle(p.id)} />
            ))}
          </div>
        </>
      )}

      <h3 className="section-title">{connected.length ? t("providers.add") : t("providers.addFirst")}</h3>
      <div className="prov-grid">
        {available.map((p) => (
          <ProviderTile key={p.id} p={p} onConnect={() => setConnect(p)} />
        ))}
        <button type="button" className="prov-tile prov-tile--custom" onClick={() => setConnect("custom")}>
          <span className="prov-mark prov-mark--ghost" aria-hidden>
            <Icon name="plus" size={14} />
          </span>
          <span className="prov-tile__text">
            <span className="prov-tile__name">{t("providers.custom")}</span>
            <span className="prov-tile__blurb">{t("providers.customBlurb")}</span>
          </span>
        </button>
      </div>

      <div className="prov-foot">
        <span className="muted">{catalog.data ? catalogText(catalog.data.source, catalog.data.fetched_at) : t("providers.catalog.loading")}</span>
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

      <Dialog
        open={!!connect}
        width={440}
        title={
          connect === "custom" ? (
            <span className="prov-dialog-title">
              <span className="prov-mark prov-mark--ghost" aria-hidden>
                <Icon name="plus" size={14} />
              </span>
              {t("providers.custom")}
            </span>
          ) : connect && meta ? (
            <span className="prov-dialog-title">
              <Mark meta={meta} />
              {t("providers.connectTitle", { name: meta.name })}
            </span>
          ) : (
            ""
          )
        }
        onClose={() => setConnect(null)}
      >
        {connect === "custom" ? (
          <CustomForm cfg={cfg} onDone={done} />
        ) : connect ? (
          <ConnectForm p={connect} cfg={cfg[connect.id] ?? {}} onDone={(ok) => done(ok ? connect.id : null)} onCancel={() => setConnect(null)} />
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

function Mark({ meta, ghost }: { meta: Meta; ghost?: boolean }) {
  const letters =
    meta.mono ??
    meta.name
      .split(/\s+/)
      .slice(0, 2)
      .map((w) => w[0])
      .join("");
  const style = meta.hue != null && !ghost ? ({ "--mark-hue": meta.hue } as CSSProperties) : undefined;
  return (
    <span className={`prov-mark ${ghost ? "prov-mark--ghost" : ""} ${style ? "prov-mark--tint" : ""}`} style={style} aria-hidden>
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

/** The whole tile is the button: one click opens setup. */
function ProviderTile({ p, onConnect }: { p: ProviderInfo; onConnect: () => void }) {
  const meta = providerMeta(p);
  const blocked = p.status === "disabled";
  const body = (
    <>
      <Mark meta={meta} ghost={blocked} />
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
        <span className="prov-tile__go" aria-hidden>
          {t("providers.connect")}
          <Icon name="chevronRight" size={14} />
        </span>
      )}
    </>
  );
  return blocked ? (
    <div className="prov-tile is-blocked">{body}</div>
  ) : (
    <button type="button" className="prov-tile" onClick={onConnect} aria-label={t("providers.connectTitle", { name: meta.name })}>
      {body}
    </button>
  );
}

function ProviderRow({ p, models, cfg, open, onToggle }: { p: ProviderInfo; models: ModelInfo[]; cfg: Record<string, unknown>; open: boolean; onToggle: () => void }) {
  const app = useApp();
  const meta = providerMeta(p);
  const azure = isAzure(p);
  const deps = (Array.isArray(cfg.deployments) ? cfg.deployments : []) as Dep[];
  const active = p.credentials.find((c) => c.active) ?? p.credentials[0];
  const count = azure ? plural(deps.length, { one: "# deployment", other: "# deployments" }) : models.length ? plural(models.length, { one: "# model", other: "# models" }) : null;
  const sub = [active ? credText(active) : meta.keyless ? t("providers.cred.none") : null, count, meta.endpoint && p.endpoint ? host(p.endpoint) : null].filter(Boolean).join(" · ");
  const bodyId = `prov-${p.id}`;
  const needsEndpoint = meta.endpoint === "required" && !cfg.base_url && !p.endpoint;
  return (
    <div className={`prov-row ${open ? "is-open" : ""} ${p.status === "error" ? "is-error" : ""}`}>
      <button type="button" className="prov-row__head" aria-expanded={open} aria-controls={bodyId} onClick={onToggle}>
        <Mark meta={meta} />
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
          {needsEndpoint && <Banner tone="warning">{t("providers.needsEndpoint")}</Banner>}
          {azure && !needsEndpoint && deps.length === 0 && <Banner tone="warning">{t("providers.needsDeployment")}</Banner>}
          {meta.endpoint && <EndpointField p={p} meta={meta} cfg={cfg} />}
          {azure && <Deployments p={p} deps={deps} />}
          {!meta.keyless && <Credentials p={p} meta={meta} />}
          {!azure && <Models models={models} keyless={!!meta.keyless} />}
          <TestBar p={p} meta={meta} onRemove={META[p.id] ? undefined : () => void removeCustom(app, p.id)} />
        </div>
      )}
    </div>
  );
}

async function removeCustom(app: App, id: string) {
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
            {p.credentials.length ? (meta.microsoft ? t("providers.addCredential") : t("providers.addAnotherKey")) : t("providers.addKey")}
          </Button>
        )
      }
    >
      {p.credentials.length > 0 && (
        <ul className="plain prov-creds">
          {p.credentials.map((c) => (
            <li key={c.credential_id} className="prov-cred">
              {p.credentials.length > 1 && <span className={`prov-radio ${c.active ? "is-on" : ""}`} aria-hidden />}
              <span className="grow">
                <span className="prov-cred__label">{isEnv(c) ? t("providers.cred.env") : c.label}</span>
                <span className="muted"> · {c.kind === "key" ? (c.hint ? `••••${c.hint}` : t("providers.apiKey")) : credText(c)}</span>
              </span>
              {p.credentials.length > 1 &&
                (c.active ? (
                  <span className="muted prov-cred__state">{t("providers.inUse")}</span>
                ) : (
                  <Button size="sm" variant="ghost" onClick={() => void app.client.provider.credential.activate({ credential_id: c.credential_id }).then(refresh, fail)}>
                    {t("providers.activate")}
                  </Button>
                ))}
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
      )}
      {adding && (
        <div className="prov-inline">
          <AddCredential p={p} meta={meta} withLabel={p.credentials.length > 0} onDone={() => setAdding(false)} />
        </div>
      )}
    </Sub>
  );
}

function EndpointField({ p, meta, cfg }: { p: ProviderInfo; meta: Meta; cfg: Record<string, unknown> }) {
  const app = useApp();
  const azure = isAzure(p);
  const current = (cfg.base_url as string | undefined) ?? "";
  const [v, setV] = useState(current);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setV(current), [current]);
  const save = async () => {
    setError(null);
    const raw = v.trim();
    let patch: Record<string, unknown> = { base_url: null };
    if (raw) {
      if (azure) {
        const parsed = parseAzureEndpoint(raw);
        if (!parsed) return setError(t("providers.err.url"));
        patch = { base_url: parsed.base };
        // A pasted Target URI carries a deployment: keep it rather than drop it.
        const deps = (Array.isArray(cfg.deployments) ? cfg.deployments : []) as Dep[];
        if (parsed.deployment && !deps.some((d) => d.name === parsed.deployment)) patch.deployments = withDep(deps, { name: parsed.deployment, model: parsed.deployment });
      } else {
        const base = normalizeBase(raw);
        if (!base) return setError(t("providers.err.url"));
        patch = { base_url: base };
      }
    }
    try {
      await saveProvider(app, p.id, patch);
      setV((patch.base_url as string | null) ?? "");
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(app.describeError(e));
    }
  };
  return (
    <Sub title={t("providers.endpoint")} action={saved && <span className="g-field__saved" role="status">{t("providers.saved")}</span>}>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          if (v !== current) void save();
        }}
      >
        <Input id={`ep-${p.id}`} aria-label={t("providers.endpoint")} className="mono" spellCheck={false} value={v} placeholder={meta.placeholder ?? p.endpoint ?? ""} onChange={(e) => setV(e.target.value)} />
        <Button type="submit" disabled={v === current}>
          {t("common.save")}
        </Button>
      </form>
      {meta.endpoint === "optional" && <p className="muted prov-hint">{t("providers.endpointDefault", { url: meta.placeholder ?? "" })}</p>}
      {error && (
        <div className="error-text" role="alert">
          {error}
        </div>
      )}
    </Sub>
  );
}

function Deployments({ p, deps }: { p: ProviderInfo; deps: Dep[] }) {
  const app = useApp();
  const known = useKnownModels();
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState<Dep>({ name: "", model: "" });
  const write = (next: Dep[]) => saveProvider(app, p.id, { deployments: next }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  const showForm = adding || deps.length === 0;
  const add = async () => {
    await write(withDep(deps, resolveDep(known, draft)));
    setDraft({ name: "", model: "" });
    setAdding(false);
  };
  return (
    <Sub
      title={t("providers.deployments")}
      action={
        !showForm && (
          <Button size="sm" variant="ghost" icon="plus" onClick={() => setAdding(true)}>
            {t("providers.addDeployment")}
          </Button>
        )
      }
    >
      {deps.length > 0 && (
        <ul className="plain prov-table">
          {deps.map((d, i) => (
            <li key={`${d.name}-${i}`}>
              <code className="grow">{d.name}</code>
              {d.model && d.model !== d.name && <span className="muted">{d.model}</span>}
              <IconButton icon="x" size="sm" label={t("providers.remove")} onClick={() => void write(deps.filter((_, j) => j !== i))} />
            </li>
          ))}
        </ul>
      )}
      {showForm && (
        <form
          className="prov-inline prov-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (draft.name.trim()) void add();
          }}
        >
          <DeploymentFields id={`dep-${p.id}`} value={draft} onChange={setDraft} autoFocus={adding} />
          <div className="row end">
            {deps.length > 0 && (
              <Button variant="ghost" onClick={() => (setAdding(false), setDraft({ name: "", model: "" }))}>
                {t("common.cancel")}
              </Button>
            )}
            <Button type="submit" variant="primary" disabled={!draft.name.trim()}>
              {t("providers.addDeployment")}
            </Button>
          </div>
        </form>
      )}
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

function TestBar({ p, meta, onRemove }: { p: ProviderInfo; meta: Meta; onRemove?: () => void }) {
  const [result, run, busy] = useTest(p, meta);
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

type TestState = { ok: boolean; text: string; detail?: string } | null;

function useTest(p: Pick<ProviderInfo, "id" | "endpoint">, meta: Meta): [TestState, () => Promise<TestState>, boolean] {
  const app = useApp();
  const [result, setResult] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const run = async (): Promise<TestState> => {
    setBusy(true);
    setResult(null);
    let r: TestState;
    try {
      const res = await app.client.provider.test({ provider: p.id });
      r = res.ok ? { ok: true, text: t("providers.testOk", { ms: res.latency_ms }) } : { ok: false, ...explain(res.error ?? t("providers.testFailed"), { name: meta.name, endpoint: p.endpoint, keyless: meta.keyless }) };
    } catch (e) {
      r = { ok: false, text: app.describeError(e) };
    }
    setResult(r);
    setBusy(false);
    return r;
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
    <span className={`prov-test ${result.ok ? "is-ok" : "is-bad"}`} role="status" title={result.detail}>
      <Icon name={result.ok ? "check" : "warning"} size={14} />
      {result.text}
    </span>
  );
}

async function saveProvider(app: App, id: string, patch: Record<string, unknown>) {
  const r = await app.client.config.get({ keys: ["providers"], cwd: null, scope: "user" });
  const cur = ((r.values.providers as ProvidersConfig | undefined) ?? {})[id] ?? {};
  const next: Record<string, unknown> = { ...cur, ...patch };
  for (const k of Object.keys(next)) if (next[k] == null) delete next[k];
  await app.client.config.set({ key: `providers.${id}`, value: next, scope: "user", cwd: null });
  await app.refreshLists();
}

/** A failure the user can act on, plus an optional "save anyway" escape hatch. */
interface Problem {
  text: string;
  detail?: string;
  force?: () => void;
}

function ProblemBanner({ problem }: { problem: Problem }) {
  return (
    <Banner
      tone="danger"
      actions={
        problem.force ? (
          <Button size="sm" variant="ghost" onClick={problem.force}>
            {t("providers.saveAnyway")}
          </Button>
        ) : undefined
      }
    >
      {problem.text}
      {problem.detail && <div className="muted prov-detail">{problem.detail}</div>}
    </Banner>
  );
}

// ---- Microsoft sign-in ----------------------------------------------------------------

function useMicrosoftLogin(p: ProviderInfo, onDone: () => void) {
  const app = useApp();
  const [login, setLogin] = useState<LoginStartResult | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  useEffect(() => {
    if (!login) return;
    const h = setInterval(async () => {
      try {
        const s = await app.client.provider.login.status({ login_id: login.login_id });
        if (s.state === "done") {
          setLogin(null);
          await app.refreshLists();
          onDone();
        } else if (s.state === "failed") {
          setLogin(null);
          setMsg(s.error ?? t("providers.signInFailed"));
        }
      } catch (e) {
        setLogin(null);
        setMsg(app.describeError(e));
      }
    }, 1500);
    return () => clearInterval(h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app, login]);

  const start = async (how: "pkce" | "device_code") => {
    setMsg(null);
    try {
      const r = await app.client.provider.login.start({ provider: p.id, method: how });
      setLogin(r);
      if (r.auth_url) await app.platform.openExternal(r.auth_url);
    } catch (e) {
      setMsg(app.describeError(e));
    }
  };
  return { login, msg, start, cancel: () => setLogin(null) };
}

function LoginWaiting({ login, onCancel }: { login: LoginStartResult; onCancel: () => void }) {
  return (
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
      <Button size="sm" variant="ghost" onClick={onCancel}>
        {t("common.cancel")}
      </Button>
    </div>
  );
}

// ---- key field ------------------------------------------------------------------------

function KeyField({ id, meta, value, onChange, autoFocus, optional }: { id: string; meta: Meta; value: string; onChange: (v: string) => void; autoFocus?: boolean; optional?: boolean }) {
  const app = useApp();
  return (
    <Field
      label={t("providers.apiKey")}
      htmlFor={id}
      extra={
        meta.keyUrl ? (
          <button type="button" className="prov-link" onClick={() => void app.platform.openExternal(meta.keyUrl!)}>
            {t("providers.getKey")} <Icon name="external" size={12} />
          </button>
        ) : optional ? (
          <span className="muted">{t("providers.optional")}</span>
        ) : undefined
      }
      description={t("providers.keyStored")}
    >
      <Input id={id} type="password" autoComplete="off" spellCheck={false} className="mono" value={value} onChange={(e) => onChange(e.target.value)} onPaste={(e) => {
        const text = e.clipboardData.getData("text");
        if (text && cleanKey(text) !== text) {
          e.preventDefault();
          onChange(cleanKey(text));
        }
      }} placeholder={t("providers.keyPlaceholder")} autoFocus={autoFocus} />
    </Field>
  );
}

// ---- connect --------------------------------------------------------------------------

/**
 * Setup for one provider in a single step: the fields it needs (endpoint and deployment for
 * Azure, a key for hosted APIs, nothing for Ollama), then a connection test. A key that fails
 * the test is not kept unless the user says so. Used by the Connect dialog and by first run.
 */
export function ConnectForm({ p, cfg = {}, onDone, onCancel }: { p: ProviderInfo; cfg?: Record<string, unknown>; onDone: (ok: boolean) => void; onCancel?: () => void }) {
  const meta = providerMeta(p);
  if (meta.keyless) return <LocalForm p={p} meta={meta} cfg={cfg} onDone={onDone} onCancel={onCancel} />;
  return <HostedForm p={p} meta={meta} cfg={cfg} onDone={onDone} onCancel={onCancel} />;
}

type FormProps = { p: ProviderInfo; meta: Meta; cfg: Record<string, unknown>; onDone: (ok: boolean) => void; onCancel?: () => void };

function Footer({ onCancel, children }: { onCancel?: () => void; children: ReactNode }) {
  return (
    <div className="g-dialog__footer">
      {onCancel && (
        <Button variant="ghost" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
      )}
      {children}
    </div>
  );
}

/** Ollama: nothing to type unless it runs somewhere other than the default address. */
function LocalForm({ p, meta, cfg, onDone, onCancel }: FormProps) {
  const app = useApp();
  const saved = (cfg.base_url as string | undefined) ?? "";
  const [editing, setEditing] = useState(!!saved);
  const [endpoint, setEndpoint] = useState(saved);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [, run, busy] = useTest({ id: p.id, endpoint: endpoint || meta.placeholder }, meta);

  const submit = async () => {
    setProblem(null);
    const raw = endpoint.trim();
    const base = raw ? normalizeBase(raw) : null;
    if (raw && !base) return setProblem({ text: t("providers.err.url") });
    try {
      if ((base ?? "") !== saved) await saveProvider(app, p.id, { base_url: base });
    } catch (e) {
      return setProblem({ text: app.describeError(e) });
    }
    const r = await run();
    if (r?.ok) {
      app.toast({ title: t("providers.connectedToast", { name: meta.name }), tone: "success" });
      onDone(true);
    } else if (r) setProblem({ text: r.text, detail: r.detail });
  };

  return (
    <form
      className="prov-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {editing ? (
        <Field label={t("providers.address")} htmlFor={`cf-ep-${p.id}`} description={t("providers.endpointDefault", { url: meta.placeholder ?? "" })}>
          <Input id={`cf-ep-${p.id}`} className="mono" spellCheck={false} value={endpoint} placeholder={meta.placeholder} onChange={(e) => setEndpoint(e.target.value)} autoFocus />
        </Field>
      ) : (
        <p className="prov-note">
          <span className="muted">{t("providers.localAt")}</span> <code>{meta.placeholder}</code>
          <button type="button" className="prov-link prov-link--inline" onClick={() => setEditing(true)}>
            {t("providers.change")}
          </button>
        </p>
      )}
      {problem && <ProblemBanner problem={problem} />}
      <Footer onCancel={onCancel}>
        <Button type="submit" variant="primary" disabled={busy}>
          {busy ? t("providers.checking") : t("providers.connect")}
        </Button>
      </Footer>
    </form>
  );
}

/** Hosted APIs: a key. Azure adds endpoint + deployment and offers Microsoft sign-in. */
function HostedForm({ p, meta, cfg, onDone, onCancel }: FormProps) {
  const app = useApp();
  const azure = isAzure(p);
  const known = useKnownModels();
  const savedBase = (cfg.base_url as string | undefined) ?? (meta.endpoint === "required" ? (p.endpoint ?? "") : "");
  const savedDeps = (Array.isArray(cfg.deployments) ? cfg.deployments : []) as Dep[];
  const [endpoint, setEndpoint] = useState(savedBase);
  const [dep, setDep] = useState<Dep>({ name: "", model: "" });
  const [key, setKey] = useState("");
  const [method, setMethod] = useState<"key" | "microsoft">("key");
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [, run] = useTest(p, meta);
  const ms = useMicrosoftLogin(p, () => {
    app.toast({ title: t("providers.connectedToast", { name: meta.name }), tone: "success" });
    onDone(true);
  });

  const parsed = azure ? parseAzureEndpoint(endpoint) : null;
  const needsEndpoint = meta.endpoint === "required";
  const needsDep = azure && savedDeps.length === 0;
  // Pasting a Target URI fills the deployment name.
  const onEndpoint = (v: string) => {
    setEndpoint(v);
    const d = azure ? parseAzureEndpoint(v)?.deployment : undefined;
    if (d && !dep.name) setDep({ name: d, model: matchModel(known, d).guess ?? "" });
  };
  const endpointOk = !needsEndpoint || (azure ? !!parsed : !!normalizeBase(endpoint));
  const depOk = !needsDep || !!dep.name.trim();
  const ready = endpointOk && depOk && (method === "microsoft" || !!cleanKey(key));

  /** Save endpoint and deployment (whatever the auth method). */
  const saveSettings = async (): Promise<boolean> => {
    const patch: Record<string, unknown> = {};
    if (needsEndpoint) {
      const base = azure ? parsed?.base : normalizeBase(endpoint);
      if (!base) {
        setProblem({ text: t("providers.err.url") });
        return false;
      }
      if (base !== savedBase) patch.base_url = base;
    }
    if (azure && dep.name.trim()) patch.deployments = withDep(savedDeps, resolveDep(known, dep));
    if (!Object.keys(patch).length) return true;
    try {
      await saveProvider(app, p.id, patch);
      return true;
    } catch (e) {
      setProblem({ text: app.describeError(e) });
      return false;
    }
  };

  const addKey = async (secret: string) => {
    const r = await app.client.provider.credential.add({ provider: p.id, kind: "key", label: t("providers.apiKey"), secret });
    await app.refreshLists();
    return r.credential_id;
  };

  const finish = () => {
    app.toast({ title: t("providers.connectedToast", { name: meta.name }), tone: "success" });
    onDone(true);
  };

  const submit = async () => {
    if (!ready) return;
    setProblem(null);
    setSaving(true);
    try {
      if (!(await saveSettings())) return;
      if (method === "microsoft") return void (await ms.start("pkce"));
      const secret = cleanKey(key);
      const cred = await addKey(secret);
      const r = await run();
      if (r?.ok) return finish();
      // Don't keep a key that doesn't work; let the user keep it on purpose (e.g. offline).
      await app.client.provider.credential.remove({ credential_id: cred }).catch(() => undefined);
      await app.refreshLists();
      setProblem({
        text: r?.text ?? t("providers.testFailed"),
        detail: r?.detail,
        force: () => void addKey(secret).then(finish, (e) => setProblem({ text: app.describeError(e) })),
      });
    } catch (e) {
      setProblem({ text: app.describeError(e) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="prov-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {azure && <p className="muted prov-hint">{t("providers.azureIntro")}</p>}
      {needsEndpoint && (
        <Field
          label={azure ? t("providers.azureEndpoint") : t("providers.endpoint")}
          htmlFor={`cf-ep-${p.id}`}
          description={azure && parsed && endpoint.trim() !== parsed.base ? t("providers.willUse", { url: parsed.base }) : azure ? t("providers.azureEndpointHint") : t("providers.endpointHint")}
          error={endpoint.trim() && !endpointOk ? t("providers.err.url") : null}
        >
          <Input id={`cf-ep-${p.id}`} className="mono" spellCheck={false} autoComplete="off" value={endpoint} placeholder={meta.placeholder} onChange={(e) => onEndpoint(e.target.value)} autoFocus={!savedBase} />
        </Field>
      )}

      {method === "key" ? (
        <KeyField id={`key-${p.id}`} meta={meta} value={key} onChange={setKey} autoFocus={!needsEndpoint || !!savedBase} />
      ) : ms.login ? (
        <LoginWaiting login={ms.login} onCancel={ms.cancel} />
      ) : (
        <p className="muted prov-hint">{t("providers.signInHint")}</p>
      )}

      {needsDep && <DeploymentFields id={`cf-dep-${p.id}`} value={dep} onChange={setDep} />}

      {meta.microsoft && !ms.login && (
        <div className="prov-alt">
          {method === "key" ? (
            <button type="button" className="prov-link prov-link--inline" onClick={() => (setMethod("microsoft"), setProblem(null))}>
              {t("providers.useMicrosoft")}
            </button>
          ) : (
            <>
              <button type="button" className="prov-link prov-link--inline" onClick={() => (setMethod("key"), setProblem(null))}>
                {t("providers.useKey")}
              </button>
              <span className="muted">·</span>
              <button type="button" className="prov-link prov-link--inline" disabled={!endpointOk || !depOk} onClick={() => void saveSettings().then((ok) => {
                  if (ok) void ms.start("device_code");
                })}>
                {t("providers.deviceCode")}
              </button>
            </>
          )}
        </div>
      )}

      {problem && <ProblemBanner problem={problem} />}
      {ms.msg && <ProblemBanner problem={{ text: ms.msg }} />}

      {!ms.login && (
        <Footer onCancel={onCancel}>
          <Button type="submit" variant="primary" icon={method === "microsoft" ? "external" : undefined} disabled={!ready || saving}>
            {saving ? t("providers.checking") : method === "microsoft" ? t("providers.signIn") : t("providers.connect")}
          </Button>
        </Footer>
      )}
    </form>
  );
}

/** Another key (or Microsoft account) for a provider that's already connected. */
function AddCredential({ p, meta, withLabel, onDone }: { p: ProviderInfo; meta: Meta; withLabel: boolean; onDone: () => void }) {
  const app = useApp();
  const [method, setMethod] = useState<"key" | "microsoft">("key");
  const [key, setKey] = useState("");
  const [label, setLabel] = useState("");
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [, run] = useTest(p, meta);
  const ms = useMicrosoftLogin(p, onDone);

  const add = async (secret: string) => {
    const r = await app.client.provider.credential.add({ provider: p.id, kind: "key", label: label.trim() || t("providers.apiKey"), secret });
    await app.refreshLists();
    return r.credential_id;
  };

  const submit = async () => {
    const secret = cleanKey(key);
    if (!secret) return;
    setProblem(null);
    setSaving(true);
    try {
      const cred = await add(secret);
      const r = await run();
      if (r?.ok) return onDone();
      await app.client.provider.credential.remove({ credential_id: cred }).catch(() => undefined);
      await app.refreshLists();
      setProblem({ text: r?.text ?? t("providers.testFailed"), detail: r?.detail, force: () => void add(secret).then(onDone, (e) => setProblem({ text: app.describeError(e) })) });
    } catch (e) {
      setProblem({ text: app.describeError(e) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="prov-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {method === "key" ? (
        <>
          <KeyField id={`add-key-${p.id}`} meta={meta} value={key} onChange={setKey} autoFocus />
          {withLabel && (
            <Field label={t("providers.label")} htmlFor={`label-${p.id}`} extra={<span className="muted">{t("providers.optional")}</span>}>
              <Input id={`label-${p.id}`} value={label} onChange={(e) => setLabel(e.target.value)} placeholder={t("providers.labelPlaceholder")} />
            </Field>
          )}
        </>
      ) : ms.login ? (
        <LoginWaiting login={ms.login} onCancel={ms.cancel} />
      ) : (
        <p className="muted prov-hint">{t("providers.signInHint")}</p>
      )}
      {meta.microsoft && !ms.login && (
        <div className="prov-alt">
          <button type="button" className="prov-link prov-link--inline" onClick={() => setMethod(method === "key" ? "microsoft" : "key")}>
            {method === "key" ? t("providers.useMicrosoft") : t("providers.useKey")}
          </button>
          {method === "microsoft" && (
            <>
              <span className="muted">·</span>
              <button type="button" className="prov-link prov-link--inline" onClick={() => void ms.start("device_code")}>
                {t("providers.deviceCode")}
              </button>
            </>
          )}
        </div>
      )}
      {problem && <ProblemBanner problem={problem} />}
      {ms.msg && <ProblemBanner problem={{ text: ms.msg }} />}
      {!ms.login && (
        <div className="row end">
          <Button variant="ghost" onClick={onDone}>
            {t("common.cancel")}
          </Button>
          {method === "key" ? (
            <Button type="submit" variant="primary" disabled={!cleanKey(key) || saving}>
              {saving ? t("providers.checking") : t("providers.addKey")}
            </Button>
          ) : (
            <Button variant="primary" icon="external" onClick={() => void ms.start("pkce")}>
              {t("providers.signIn")}
            </Button>
          )}
        </div>
      )}
    </form>
  );
}

/**
 * An OpenAI-compatible endpoint (vLLM, LM Studio, a gateway…). The URL is the only required
 * field; the name defaults to the host and the key is optional. Tested before it's kept.
 */
function CustomForm({ cfg, onDone }: { cfg: ProvidersConfig; onDone: (id: string | null) => void }) {
  const app = useApp();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [problem, setProblem] = useState<Problem | null>(null);
  const [saving, setSaving] = useState(false);
  const base = normalizeBase(url);
  const fallbackName = base ? host(base).replace(/:\d+$/, (m) => ` ${m.slice(1)}`) : "";
  const display = name.trim() || fallbackName;
  const id = display
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  const taken = !!id && (id in cfg || app.providers.some((p) => p.id === id));
  const meta: Meta = { name: display, blurb: "providers.blurb.custom" };
  const [, run] = useTest({ id, endpoint: base }, meta);

  const create = async () => {
    await app.client.config.set({ key: `providers.${id}`, value: { kind: "openai-compatible", base_url: base }, scope: "user", cwd: null });
    await app.refreshLists();
    const secret = cleanKey(key);
    if (secret) await app.client.provider.credential.add({ provider: id, kind: "key", label: t("providers.apiKey"), secret });
    await app.refreshLists();
  };
  const finish = () => {
    app.toast({ title: t("providers.connectedToast", { name: display }), tone: "success" });
    onDone(id);
  };

  const save = async () => {
    setProblem(null);
    setSaving(true);
    try {
      await create();
      const r = await run();
      if (r?.ok) return finish();
      await removeCustom(app, id);
      setProblem({ text: r?.text ?? t("providers.testFailed"), detail: r?.detail, force: () => void create().then(finish, (e) => setProblem({ text: app.describeError(e) })) });
    } catch (e) {
      setProblem({ text: app.describeError(e) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="prov-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (id && !taken && base) void save();
      }}
    >
      <p className="muted prov-hint">{t("providers.customHint")}</p>
      <Field label={t("providers.endpoint")} htmlFor="custom-url" error={url.trim() && !base ? t("providers.err.url") : null}>
        <Input id="custom-url" className="mono" spellCheck={false} autoComplete="off" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://localhost:1234/v1" autoFocus />
      </Field>
      <Field label={t("providers.customName")} htmlFor="custom-name" extra={<span className="muted">{t("providers.optional")}</span>} error={taken ? t("providers.customTaken") : null}>
        <Input id="custom-name" value={name} onChange={(e) => setName(e.target.value)} placeholder={fallbackName || "LM Studio"} />
      </Field>
      <KeyField id="custom-key" meta={meta} value={key} onChange={setKey} optional />
      {problem && <ProblemBanner problem={problem} />}
      <Footer onCancel={() => onDone(null)}>
        <Button type="submit" variant="primary" disabled={!id || taken || !base || saving}>
          {saving ? t("providers.checking") : t("providers.addProvider")}
        </Button>
      </Footer>
    </form>
  );
}
