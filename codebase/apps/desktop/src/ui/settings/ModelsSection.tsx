// Settings → Models: every chat model from a connected provider with a switch that keeps it
// in or out of the model pickers (`hiddenModels`, user scope). Defaults (model, small model,
// effort) are the schema card above this one.
import { useEffect, useState } from "react";
import { Button, Icon, IconButton, SettingRow, SettingsGroup, Switch } from "@genie/ui";
import { t } from "../../i18n";
import { errorKind } from "../../platform";
import { useApp } from "../context";
import { providerMeta } from "./ProvidersSection";

export function ModelsSection() {
  const app = useApp();
  const [q, setQ] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [locked, setLocked] = useState(false);
  const models = app.chatReadyModels;
  const hidden = app.hiddenModels;

  useEffect(() => {
    const load = () =>
      app.client.config
        .get({ keys: ["hiddenModels"], cwd: null, scope: null })
        .then((r) => setLocked(r.locked.includes("hiddenModels")))
        .catch(() => setLocked(false));
    void load();
    return app.onConfigChanged(() => void load());
  }, [app]);

  const fail = (e: unknown) => app.toast({ title: errorKind(e) === "locked" ? t("permissions.managed") : app.describeError(e), tone: "danger" });
  const save = (next: Set<string>) =>
    void app.client.config
      .set({ key: "hiddenModels", value: [...next].sort(), scope: "user", cwd: null })
      .then(() => app.refreshLists())
      .catch(fail);
  const toggle = (id: string, on: boolean) => {
    const next = new Set(hidden);
    if (on) next.delete(id);
    else next.add(id);
    save(next);
  };

  const refresh = async () => {
    setRefreshing(true);
    try {
      await app.client.catalog.refresh({});
      await app.refreshLists();
    } catch (e) {
      fail(e);
    } finally {
      setRefreshing(false);
    }
  };

  const needle = q.trim().toLowerCase();
  const providerName = (id: string) => providerMeta({ id, kind: id }).name;
  const shown = needle ? models.filter((m) => [m.name, m.id, providerName(m.provider)].some((s) => s.toLowerCase().includes(needle))) : models;
  const visibleCount = models.filter((m) => !hidden.has(m.id)).length;
  const someHidden = models.some((m) => hidden.has(m.id));

  return (
    <SettingsGroup
      title={t("models.list")}
      action={
        someHidden && !locked ? (
          <Button size="sm" variant="ghost" onClick={() => save(new Set([...hidden].filter((id) => !models.some((m) => m.id === id))))}>
            {t("models.showAll")}
          </Button>
        ) : undefined
      }
    >
      <div className="models-search">
        <label className="search">
          <Icon name="search" size={14} />
          <span className="g-sr-only">{t("models.search")}</span>
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("models.search")} />
        </label>
        <IconButton icon="refresh" size="sm" label={t("models.refresh")} disabled={refreshing} onClick={() => void refresh()} />
      </div>
      {locked && (
        <p className="muted">
          <Icon name="lock" size={13} /> {t("permissions.managed")}
        </p>
      )}
      {!models.length && (
        <div className="models-empty">
          <p className="muted">{t("models.none")}</p>
          <Button size="sm" onClick={() => app.setUi({ settingsSection: "providers" })}>
            {t("composer.connectProvider")}
          </Button>
        </div>
      )}
      {models.length > 0 && !shown.length && <p className="muted">{t("models.noMatch", { q })}</p>}
      {shown.map((m) => {
        const on = !hidden.has(m.id);
        // The pickers always keep at least one model.
        const last = on && visibleCount <= 1;
        return (
          <SettingRow key={m.id} label={m.name || m.id} description={providerName(m.provider)}>
            <span title={last ? t("models.lastOne") : undefined}>
              <Switch checked={on} disabled={locked || last} aria-label={m.name || m.id} onChange={(v) => toggle(m.id, v)} />
            </span>
          </SettingRow>
        );
      })}
    </SettingsGroup>
  );
}
