// First run (05 §5): Welcome → Privacy consent → Connect a provider (skippable) → Import
// (if found) → Memory notice. The trust dialog comes with the first Code folder; the
// passphrase step is the Unlock screen when the engine reports a locked store.
import { useEffect, useState } from "react";
import { Button, Card, Select, Switch } from "@genie/ui";
import { t } from "../i18n";
import { useApp } from "./context";
import { GenieMark } from "./GenieMark";
import { ProviderCard } from "./settings/ProvidersSection";

export type Step = "welcome" | "privacy" | "provider" | "import" | "memory";
export const STEPS: Step[] = ["welcome", "privacy", "provider", "import", "memory"];

/** The next step; `import` is skipped when nothing was found. */
export function nextStep(cur: Step, importFound: boolean): Step | null {
  const i = STEPS.indexOf(cur);
  let n = STEPS[i + 1] ?? null;
  if (n === "import" && !importFound) n = "memory";
  return n;
}

function hasImportItems(report: unknown): boolean {
  const r = report as { items?: unknown[]; sources?: unknown[] } | null;
  return !!r && ((Array.isArray(r.items) && r.items.length > 0) || (Array.isArray(r.sources) && r.sources.length > 0));
}

export function FirstRun() {
  const app = useApp();
  const [step, setStep] = useState<Step>("welcome");
  const [crash, setCrash] = useState(true);
  const [usage, setUsage] = useState(false);
  const [locked, setLocked] = useState<string[]>([]);
  const [report, setReport] = useState<unknown>(null);
  const [provider, setProvider] = useState<string>("");

  useEffect(() => {
    app.client.config
      .get({ keys: ["telemetry.crashReports", "telemetry.usage"] })
      .then((r) => {
        setLocked(r.locked);
        if (typeof r.values["telemetry.crashReports"] === "boolean") setCrash(r.values["telemetry.crashReports"] as boolean);
        if (typeof r.values["telemetry.usage"] === "boolean") setUsage(r.values["telemetry.usage"] as boolean);
      })
      .catch(() => {});
    app.client.import
      .scan({})
      .then((r) => setReport(r.report))
      .catch(() => setReport(null));
  }, [app]);

  const found = hasImportItems(report);
  const next = async () => {
    if (step === "privacy") {
      const set = (key: string, value: boolean) => (locked.includes(key) ? Promise.resolve() : app.client.config.set({ key, value, scope: "user" }).catch(() => {}));
      await Promise.all([set("telemetry.crashReports", crash), set("telemetry.usage", usage)]);
    }
    const n = nextStep(step, found);
    if (n) setStep(n);
    else app.setUi({ firstRunDone: true });
  };
  const p = app.providers.find((x) => x.id === provider) ?? app.providers[0];

  return (
    <div className="center-screen">
      <Card className="firstrun">
        <div className="firstrun__steps" aria-hidden>
          {STEPS.filter((s) => s !== "import" || found).map((s) => (
            <span key={s} className={`firstrun__dot ${s === step ? "is-active" : ""}`} />
          ))}
        </div>
        {step === "welcome" && (
          <>
            <h1 className="firstrun__title">
              <span className="hero__mark" aria-hidden>
                <GenieMark size={36} />
              </span>
              {t("firstrun.welcome")}
            </h1>
            <p>{t("firstrun.welcomeBody")}</p>
          </>
        )}
        {step === "privacy" && (
          <>
            <h2>{t("firstrun.privacy")}</h2>
            <p>{t("firstrun.privacyBody")}</p>
            <div className="stack">
              <Switch checked={crash} disabled={locked.includes("telemetry.crashReports")} onChange={setCrash} label={t("firstrun.crash")} />
              <Switch checked={usage} disabled={locked.includes("telemetry.usage")} onChange={setUsage} label={t("firstrun.usage")} />
            </div>
          </>
        )}
        {step === "provider" && (
          <>
            <h2>{t("firstrun.provider")}</h2>
            <p className="muted">{t("firstrun.providerBody")}</p>
            {app.providers.length > 1 && (
              <Select aria-label={t("firstrun.preset")} value={p?.id ?? ""} onChange={setProvider} options={app.providers.map((x) => ({ value: x.id, label: x.id }))} />
            )}
            {p ? <ProviderCard p={p} compact /> : <p className="muted">{t("providers.none")}</p>}
          </>
        )}
        {step === "import" && (
          <>
            <h2>{t("firstrun.import")}</h2>
            <p>{t("firstrun.importBody")}</p>
            <pre className="code">{JSON.stringify(report, null, 2)}</pre>
            <Button onClick={() => void app.client.import.apply({}).then(() => app.toast({ title: "Imported", tone: "success" }), (e) => app.toast({ title: app.describeError(e), tone: "danger" }))}>Import</Button>
          </>
        )}
        {step === "memory" && (
          <>
            <h2>{t("firstrun.memory")}</h2>
            <p>{t("firstrun.memoryBody")}</p>
          </>
        )}
        <div className="row end">
          {(step === "provider" || step === "import") && (
            <Button variant="ghost" onClick={() => void next()}>
              {t("firstrun.skip")}
            </Button>
          )}
          <Button variant="primary" onClick={() => void next()}>
            {step === "welcome" ? t("firstrun.start") : step === "memory" ? t("firstrun.done") : t("firstrun.next")}
          </Button>
        </div>
      </Card>
    </div>
  );
}
