// Session details column at the window's right edge: where the session runs (environment +
// folder), how full its context window is, and the state of MCP connectors.
import { useState } from "react";
import { Icon } from "@genie/ui";
import { t } from "../../i18n";
import { useApp, useAsync, useSession } from "../context";
import { folderName } from "../Sidebar";
import { ContextBreakdown } from "../chat/ContextMeter";
import { DockToggles } from "./RightPanel";

export function DetailsPane() {
  const app = useApp();
  const { ui } = app;
  const sid = ui.activeSession;
  const store = useSession(sid);
  const info = sid ? store?.model.info : undefined;
  const [ctxOpen, setCtxOpen] = useState(false);
  // Same list as Settings → Connectors; refreshed when the pane opens or the session changes.
  const mcp = useAsync(() => app.client.mcp.list({}), [sid]);

  const code = ui.mode === "code";
  const workspace = code ? (sid ? (info?.workspace_id ?? "") : ui.codeWorkspace) : "";
  const project = !code ? app.projects.find((p) => p.project_id === (sid ? info?.project_id : ui.activeProject)) : undefined;

  const usage = sid ? store?.model.usage : undefined;
  const ctx = usage?.context_window ? Math.min(1, usage.context_tokens / usage.context_window) : null;

  const servers = mcp.data?.servers ?? [];
  const broken = servers.filter((s) => s.status === "needs_auth" || s.status === "failed").length;
  const connected = servers.filter((s) => s.status === "connected").length;
  const connectorsNote = broken ? t("details.reconnect", { n: broken }) : servers.length ? t("details.connected", { n: connected }) : t("details.none");

  return (
    <aside className="details" aria-label={t("details.title")}>
      <div className="details__head" data-tauri-drag-region>
        <span className="grow" data-tauri-drag-region />
        <DockToggles />
      </div>
      <div className="details__body">
        <section className="details__section">
          <h3 className="details__heading">{t("details.environment")}</h3>
          <div className="details__row">
            <Icon name="computer" />
            <span className="details__text">{t("code.thisComputer")}</span>
          </div>
          {code ? (
            workspace ? (
              <button className="details__row details__row--link" title={t("details.reveal", { path: workspace })} onClick={() => void app.platform.openPath(workspace).catch(() => {})}>
                <Icon name="folder" />
                <span className="details__text">{folderName(workspace)}</span>
              </button>
            ) : (
              <div className="details__row">
                <Icon name="folder" />
                <span className="details__text">{t("code.noProject")}</span>
              </div>
            )
          ) : (
            <div className="details__row">
              <Icon name="folder" />
              <span className="details__text">{project?.name ?? t("row.noProject")}</span>
            </div>
          )}
        </section>

        <section className="details__section">
          <button className="details__link" aria-expanded={ctxOpen} disabled={!store || ctx == null} onClick={() => setCtxOpen((v) => !v)}>
            <span>{t("details.context")}</span>
            <Icon name="chevronRight" size={12} />
            <span className="grow" />
            {ctx == null ? (
              <span className="details__note">—</span>
            ) : (
              <>
                <span className="details__note">{Math.round(ctx * 100)}%</span>
                <span className={`details__meter ${ctx > 0.8 ? "is-warn" : ""}`} aria-hidden>
                  <span style={{ width: `${Math.max(8, ctx * 100)}%` }} />
                </span>
              </>
            )}
          </button>
          {ctxOpen && store && <div className="details__ctx"><ContextBreakdown store={store} /></div>}
        </section>

        <section className="details__section">
          <button className="details__link" onClick={() => app.openDeepLink("genie://settings/connectors")}>
            <span>{t("details.connectors")}</span>
            <Icon name="chevronRight" size={12} />
            <span className="grow" />
            <span className={`details__note ${broken ? "is-warn" : ""}`}>{mcp.loading && !mcp.data ? "" : connectorsNote}</span>
          </button>
        </section>
      </div>
    </aside>
  );
}
