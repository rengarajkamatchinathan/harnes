// Right panel (05 §1, D54): Chat → Artifact · Browser; Code → Changes · Files · Terminal ·
// Browser. Resizable and hideable; width and tab live in state.json.
import { useRef } from "react";
import { EmptyState, IconButton, type IconName } from "@genie/ui";
import { t } from "../../i18n";
import type { PanelTab } from "../../state/app";
import { useApp } from "../context";
import { ArtifactPanel } from "./ArtifactPanel";
import { BrowserPanel } from "./BrowserPanel";
import { ChangesPanel, FilesPanel } from "./CodePanels";
import { TerminalPanel } from "./TerminalPanel";

export const PANEL_ICON: Record<PanelTab, IconName> = { artifact: "artifacts", browser: "globe", changes: "branch", files: "folder", terminal: "terminal" };

export function panelTabs(mode: string): PanelTab[] {
  return mode === "code" ? ["changes", "files", "terminal", "browser"] : ["artifact", "browser"];
}

/** The tab actually shown: the saved one if this mode has it, else the mode's first. */
export function effectiveTab(mode: string, saved: PanelTab): PanelTab {
  const tabs = panelTabs(mode);
  return tabs.includes(saved) ? saved : tabs[0];
}

export function RightPanel() {
  const app = useApp();
  const tab = effectiveTab(app.ui.mode, app.ui.panelTab);
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
      <div className="panel__head" data-tauri-drag-region>
        {/* The header's icon buttons switch tabs (D54); the panel just names what it shows. */}
        <h2 className="panel__title">{t(`panel.${tab}` as Parameters<typeof t>[0])}</h2>
        <span className="grow" data-tauri-drag-region />
        <IconButton
          icon="expand"
          size="sm"
          label={t("artifact.fullscreen")}
          onClick={() => {
            const max = Math.max(280, Math.min(1400, window.innerWidth - (app.ui.sidebarCollapsed ? 0 : app.ui.sidebarWidth) - 360));
            app.setUi({ panelWidth: app.ui.panelWidth >= max - 10 ? 600 : max });
          }}
        />
        <IconButton icon="x" size="sm" label={t("panel.hide")} onClick={() => app.setUi({ panelOpen: false })} />
      </div>
      <div className="panel__body">
        {!sid ? (
          <EmptyState icon={PANEL_ICON[tab]} title={t("panel.noSessionTitle")} hint={t("panel.noSession")} />
        ) : tab === "artifact" ? (
          <ArtifactPanel sessionId={sid} />
        ) : tab === "browser" ? (
          <BrowserPanel sessionId={sid} />
        ) : tab === "changes" ? (
          <ChangesPanel sessionId={sid} />
        ) : tab === "files" ? (
          <FilesPanel sessionId={sid} />
        ) : (
          <TerminalPanel sessionId={sid} />
        )}
      </div>
    </aside>
  );
}
