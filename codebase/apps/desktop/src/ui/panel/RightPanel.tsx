// Right panel (05 §1, D54): Chat → Artifact · Browser; Code → Changes · Files · Terminal ·
// Browser. Resizable and hideable; width and tab live in state.json.
import { useRef } from "react";
import { IconButton, Tabs } from "@genie/ui";
import { t } from "../../i18n";
import type { PanelTab } from "../../state/app";
import { useApp } from "../context";
import { ArtifactPanel } from "./ArtifactPanel";
import { BrowserPanel } from "./BrowserPanel";
import { ChangesPanel, FilesPanel, TerminalPanel } from "./CodePanels";

export function panelTabs(mode: string): PanelTab[] {
  return mode === "code" ? ["changes", "files", "terminal", "browser"] : ["artifact", "browser"];
}

export function RightPanel() {
  const app = useApp();
  const tabs = panelTabs(app.ui.mode);
  const tab = tabs.includes(app.ui.panelTab) ? app.ui.panelTab : tabs[0];
  const sid = app.ui.activeSession;
  const drag = useRef<{ x: number; w: number } | null>(null);

  return (
    <aside className="panel" aria-label="Panel">
      <div
        className="panel__resize"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize panel"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "ArrowLeft") app.setUi({ panelWidth: Math.min(900, app.ui.panelWidth + 20) });
          if (e.key === "ArrowRight") app.setUi({ panelWidth: Math.max(280, app.ui.panelWidth - 20) });
        }}
        onPointerDown={(e) => {
          drag.current = { x: e.clientX, w: app.ui.panelWidth };
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (!drag.current) return;
          app.setUi({ panelWidth: Math.max(280, Math.min(900, drag.current.w + drag.current.x - e.clientX)) });
        }}
        onPointerUp={() => (drag.current = null)}
      />
      <div className="panel__head">
        <Tabs label="Panel" value={tab} onChange={(id) => app.setUi({ panelTab: id as PanelTab })} tabs={tabs.map((id) => ({ id, label: t(`panel.${id}` as Parameters<typeof t>[0]) }))} />
        <span className="grow" />
        <IconButton icon="x" label={t("panel.hide")} onClick={() => app.setUi({ panelOpen: false })} />
      </div>
      <div className="panel__body" role="tabpanel">
        {!sid ? (
          <div className="empty muted">{tab === "artifact" ? t("artifact.none") : t("browser.idle")}</div>
        ) : tab === "artifact" ? (
          <ArtifactPanel sessionId={sid} />
        ) : tab === "browser" ? (
          <BrowserPanel sessionId={sid} />
        ) : tab === "changes" ? (
          <ChangesPanel sessionId={sid} />
        ) : tab === "files" ? (
          <FilesPanel sessionId={sid} />
        ) : (
          <TerminalPanel />
        )}
      </div>
    </aside>
  );
}
