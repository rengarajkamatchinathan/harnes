// Providers & Models (05 §4, 03 §10): credentials (source shown), API key, Sign in with
// Microsoft (PKCE in the system browser, or device code), test, models, catalog status.
import { useEffect, useState } from "react";
import type { LoginStartResult, ProviderInfo } from "@genie/protocol";
import { Badge, Button, Card, Input } from "@genie/ui";
import { t } from "../../i18n";
import { useApp, useAsync } from "../context";

const TONE = { ready: "success", needs_auth: "warning", error: "danger", disabled: "neutral", unknown: "neutral" } as const;

export function ProvidersSection() {
  const app = useApp();
  const catalog = useAsync(() => app.client.catalog.status({}), []);
  return (
    <div className="stack">
      {catalog.data && (
        <div className="row muted">
          {t("providers.catalog", { source: catalog.data.source, when: new Date(catalog.data.fetched_at).toLocaleString() })}
          <Button size="sm" icon="refresh" onClick={() => void app.client.catalog.refresh({}).then(() => catalog.reload(), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>
            {t("providers.refresh")}
          </Button>
        </div>
      )}
      {app.providers.map((p) => (
        <ProviderCard key={p.id} p={p} />
      ))}
      {!app.providers.length && <p className="muted">{t("providers.none")}</p>}
    </div>
  );
}

export function ProviderCard({ p, compact }: { p: ProviderInfo; compact?: boolean }) {
  const app = useApp();
  const [label, setLabel] = useState("");
  const [key, setKey] = useState("");
  const [test, setTest] = useState<string | null>(null);
  const [login, setLogin] = useState<LoginStartResult | null>(null);
  const [loginState, setLoginState] = useState<string | null>(null);
  const models = app.models.filter((m) => m.provider === p.id);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const refresh = () => app.refreshLists();

  // Poll the sign-in until done/failed.
  useEffect(() => {
    if (!login) return;
    const h = setInterval(async () => {
      try {
        const s = await app.client.provider.login.status({ login_id: login.login_id });
        if (s.state === "done") {
          setLoginState(t("providers.signedIn"));
          setLogin(null);
          void refresh();
        } else if (s.state === "failed") {
          setLoginState(s.error ?? "failed");
          setLogin(null);
        }
      } catch (e) {
        setLoginState(app.describeError(e));
        setLogin(null);
      }
    }, 1500);
    return () => clearInterval(h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app, login]);

  const signIn = async (method: "pkce" | "device_code") => {
    try {
      const r = await app.client.provider.login.start({ provider: p.id, method });
      setLogin(r);
      setLoginState(t("providers.waiting"));
      if (r.auth_url) await app.platform.openExternal(r.auth_url);
    } catch (e) {
      fail(e);
    }
  };
  const addKey = async () => {
    try {
      await app.client.provider.credential.add({ provider: p.id, kind: "key", label: label || "API key", secret: key });
      setKey("");
      setLabel("");
      await refresh();
      await runTest();
    } catch (e) {
      fail(e);
    }
  };
  const runTest = async () => {
    setTest(t("firstrun.testing"));
    try {
      const r = await app.client.provider.test({ provider: p.id });
      setTest(r.ok ? t("providers.testOk", { ms: r.latency_ms }) : (r.error ?? "failed"));
    } catch (e) {
      setTest(app.describeError(e));
    }
  };
  const microsoft = /azure|foundry|entra|microsoft/i.test(`${p.id} ${p.kind}`);

  return (
    <Card>
      <div className="row">
        <strong>{p.id}</strong>
        <span className="muted">{p.kind}</span>
        <Badge tone={TONE[p.status] ?? "neutral"}>{t(`providers.status.${p.status === "unknown" ? "error" : p.status}` as Parameters<typeof t>[0])}</Badge>
        <span className="grow" />
        <Button size="sm" onClick={() => void runTest()}>
          {t("providers.test")}
        </Button>
      </div>
      {test && (
        <div className="muted" role="status">
          {test}
        </div>
      )}
      {p.endpoint && <div className="muted">{p.endpoint}</div>}
      {p.credentials.map((c) => (
        <div key={c.credential_id} className="row">
          <span className="grow">
            {c.label} <span className="muted">· {c.kind}{c.hint ? ` · …${c.hint}` : ""}</span>
          </span>
          {c.active ? (
            <Badge tone="success">{t("providers.active")}</Badge>
          ) : (
            <Button size="sm" onClick={() => void app.client.provider.credential.activate({ credential_id: c.credential_id }).then(refresh, fail)}>
              {t("providers.activate")}
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => void app.client.provider.credential.remove({ credential_id: c.credential_id }).then(refresh, fail)}>
            {t("providers.remove")}
          </Button>
        </div>
      ))}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          void addKey();
        }}
      >
        {!compact && <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder={t("providers.label")} aria-label={t("providers.label")} style={{ maxWidth: 140 }} />}
        <Input type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value)} placeholder={t("providers.key")} aria-label={t("providers.key")} />
        <Button type="submit" disabled={!key}>
          {t("providers.addKey")}
        </Button>
      </form>
      {microsoft && (
        <div className="row">
          <Button onClick={() => void signIn("pkce")}>{t("providers.signIn")}</Button>
          <Button variant="ghost" onClick={() => void signIn("device_code")}>
            {t("providers.deviceCode")}
          </Button>
        </div>
      )}
      {login?.user_code && <div>{t("providers.deviceCodeBody", { uri: login.verification_uri ?? "", code: login.user_code })}</div>}
      {loginState && <div className="muted">{loginState}</div>}
      {!compact && models.length > 0 && (
        <details>
          <summary>
            {t("providers.models")} ({models.length})
          </summary>
          <ul className="plain">
            {models.map((m) => (
              <li key={m.id}>
                <code>{m.id}</code> <span className="muted">{m.context_window ? `${Math.round(m.context_window / 1000)}k ctx` : ""}{m.effort_levels?.length ? ` · effort ${m.effort_levels.join("/")}` : ""}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </Card>
  );
}
