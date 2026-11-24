// Background tasks (panel tab): what the session has running besides the conversation —
// subagents, dev servers from `.genie/launch.json`, and terminals.
import { useEffect } from "react";
import { Button, EmptyState, Icon } from "@genie/ui";
import { formatCost, t } from "../../i18n";
import { useEmitter } from "../../state/emitter";
import { useApp, useSession } from "../context";

export function TasksPanel({ sessionId }: { sessionId: string }) {
  const app = useApp();
  const store = useSession(sessionId);
  useEmitter(app.devServers);
  useEmitter(app.terminals);
  useEffect(() => app.devServers.ensure(sessionId), [app, sessionId]);

  const subagents = store ? [...store.model.subagents.values()] : [];
  const servers = app.devServers.get(sessionId).servers;
  const terminals = app.terminals.list(sessionId);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const openUrl = (url: string) => {
    app.setUi({ panelTab: "browser" });
    void app.client.browser.tab.new({ session_id: sessionId, url }).catch(fail);
  };

  if (!subagents.length && !servers.length && !terminals.length) return <EmptyState icon="layers" title={t("tasks.noneTitle")} hint={t("tasks.none")} />;

  return (
    <div className="tasks">
      {subagents.length > 0 && (
        <section className="tasks__section">
          <h3 className="tasks__heading">{t("tasks.subagents")}</h3>
          {subagents.map((s) => (
            <div key={s.childSessionId} className="tasks__row">
              <span className={`tasks__dot tasks__dot--${s.status}`} aria-hidden />
              <div className="tasks__main">
                <span className="tasks__title">{s.description || s.agent}</span>
                <span className="tasks__sub">
                  {s.agent} · {t(`tasks.status.${s.status}` as Parameters<typeof t>[0])}
                  {s.costUsd > 0 ? ` · ${formatCost(s.costUsd)}` : ""}
                </span>
              </div>
              {s.status === "running" && <span className="g-spinner" aria-hidden />}
            </div>
          ))}
        </section>
      )}
      {servers.length > 0 && (
        <section className="tasks__section">
          <h3 className="tasks__heading">{t("tasks.servers")}</h3>
          {servers.map((s) => (
            <div key={s.name} className="tasks__row">
              <span className={`tasks__dot tasks__dot--${s.state}`} aria-hidden />
              <div className="tasks__main">
                <span className="tasks__title">{s.name}</span>
                <span className="tasks__sub">{s.error ?? s.url ?? s.command ?? s.state}</span>
              </div>
              {s.url && s.state === "running" && (
                <Button size="sm" variant="ghost" onClick={() => openUrl(s.url!)}>
                  {t("common.open")}
                </Button>
              )}
              {(s.state === "running" || s.state === "starting") && (
                <Button size="sm" variant="ghost" onClick={() => void app.client.preview.stop({ session_id: sessionId, name: s.name }).catch(fail)}>
                  {t("tasks.stop")}
                </Button>
              )}
            </div>
          ))}
        </section>
      )}
      {terminals.length > 0 && (
        <section className="tasks__section">
          <h3 className="tasks__heading">{t("tasks.terminals")}</h3>
          {terminals.map((term) => (
            <button key={term.id} className="tasks__row tasks__row--link" onClick={() => app.setUi({ panelTab: "terminal" })}>
              <span className={`tasks__dot tasks__dot--${term.exited ? "exited" : "running"}`} aria-hidden />
              <div className="tasks__main">
                <span className="tasks__title">{term.title}</span>
                <span className="tasks__sub">{term.exited ? t("tasks.exited", { code: term.code ?? "—" }) : t("tasks.status.running")}</span>
              </div>
              <Icon name="chevronRight" size={12} />
            </button>
          ))}
        </section>
      )}
    </div>
  );
}
