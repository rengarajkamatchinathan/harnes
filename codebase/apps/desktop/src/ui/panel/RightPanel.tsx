// Right panel (05 §1, D54): a strip of open tabs (Chat → Artifact · Browser; Code → Changes ·
// Browser · Files · Terminal) with a "+" menu to open more, and a launcher when none is open.
// Resizable and hideable; width, open tabs and the active tab live in state.json.
import { EmptyState, Icon, IconButton, Menu, type IconName, type MenuItem } from "@genie/ui";
import { t } from "../../i18n";
import type { App, PanelTab, UiState } from "../../state/app";
import { useApp } from "../context";
import { ResizeHandle } from "../ResizeHandle";
import { PANEL_DEFAULT, PANEL_MIN, useShell } from "../shell";
import { ArtifactPanel } from "./ArtifactPanel";
import { BrowserPanel } from "./BrowserPanel";
import { ChangesPanel, FilesPanel } from "./CodePanels";
import { TerminalPanel } from "./TerminalPanel";
import { SideChatPanel } from "./SideChatPanel";
import { TasksPanel } from "./TasksPanel";

export const PANEL_ICON: Record<PanelTab, IconName> = { artifact: "artifacts", browser: "globe", changes: "diff", files: "folder", terminal: "terminal", sidechat: "chat", tasks: "layers" };

const isMac = () => typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);

/** Keyboard shortcut per tab, as shown in the launcher and "+" menu (handled in Root). */
export function panelShortcut(tab: PanelTab): string | undefined {
  const mac = isMac();
  switch (tab) {
    case "changes":
      return mac ? "⇧⌘D" : "Ctrl+Shift+D";
    case "browser":
      return mac ? "⇧⌘B" : "Ctrl+Shift+B";
    case "files":
      return mac ? "⇧⌘F" : "Ctrl+Shift+F";
    case "terminal":
      return mac ? "⌘J" : "Ctrl+J";
    case "sidechat":
      return mac ? "⌘;" : "Ctrl+;";
    default:
      return undefined;
  }
}

export function panelTabs(mode: string): PanelTab[] {
  return mode === "code" ? ["changes", "browser", "files", "terminal", "sidechat", "tasks"] : ["artifact", "browser"];
}

/** Tabs that are views of the workspace; the rest (side chat, background tasks) are listed after a divider in the "+" menu. */
const WORKSPACE_TABS: PanelTab[] = ["artifact", "changes", "browser", "files", "terminal"];

/** What the right dock shows: the tabbed panel and/or the session details column. */
export function dockState(ui: Pick<UiState, "view" | "panelOpen" | "panelMinimized" | "detailsOpen">) {
  const here = ui.view !== "project" && ui.view !== "handoff";
  return { panelShown: here && ui.panelOpen && !ui.panelMinimized, detailsShown: here && ui.detailsOpen };
}

/**
 * The two toggles (session details, panel). They live at the window's top-right corner, in
 * whichever column is rightmost: the main header when nothing is open; the panel's head (panel
 * toggle) with details staying in the main header when only the panel is open; the details
 * column's head (both) when details are open.
 */
export function DockToggles({ details = true, panel = true }: { details?: boolean; panel?: boolean }) {
  const app = useApp();
  const { panelShown, detailsShown } = dockState(app.ui);
  return (
    <div className="header-icons">
      {details && <IconButton icon="list" size="sm" className={detailsShown ? "is-on" : ""} aria-pressed={detailsShown} label={t("details.toggle")} onClick={() => app.setUi({ detailsOpen: !detailsShown })} />}
      {panel && <IconButton icon="panel" size="sm" className={panelShown ? "is-on" : ""} aria-pressed={panelShown} label={t("panel.toggle")} onClick={() => app.setUi({ panelOpen: !panelShown })} />}
    </div>
  );
}

/** Open tabs this mode can show, in strip order. */
export function openTabs(ui: Pick<UiState, "mode" | "panelTabsOpen">): PanelTab[] {
  const allowed = panelTabs(ui.mode);
  return ui.panelTabsOpen.filter((tab) => allowed.includes(tab));
}

/** The tab shown: the saved one if it is open in this mode, else the last open one, else none (launcher). */
export function activeTab(ui: Pick<UiState, "mode" | "panelTab" | "panelTabsOpen">): PanelTab | null {
  const open = openTabs(ui);
  return open.includes(ui.panelTab) ? ui.panelTab : (open[open.length - 1] ?? null);
}

/** Open `tab` in the panel, or hide the panel when that tab is already the one showing. */
export function togglePanelTab(app: App, tab: PanelTab) {
  const shown = app.ui.panelOpen && !app.ui.panelMinimized && activeTab(app.ui) === tab;
  app.setUi(shown ? { panelOpen: false } : { panelOpen: true, panelTab: tab });
}

const tabLabel = (tab: PanelTab) => t(`panel.${tab}` as Parameters<typeof t>[0]);

export function RightPanel() {
  const app = useApp();
  const { ui } = app;
  const tabs = openTabs(ui);
  const tab = activeTab(ui);
  const sid = ui.activeSession;
  const shell = useShell();

  const open = (next: PanelTab) => app.setUi({ panelTab: next });
  const close = (gone: PanelTab) => {
    const rest = ui.panelTabsOpen.filter((x) => x !== gone);
    const i = tabs.indexOf(gone);
    const visible = tabs.filter((x) => x !== gone);
    // Closing the active tab shows its neighbour (the one to the left, else the right).
    app.setUi(gone === tab ? { panelTabsOpen: rest, panelTab: visible[Math.max(0, i - 1)] ?? ui.panelTab } : { panelTabsOpen: rest });
  };
  const all = panelTabs(ui.mode);
  const newTabItems: MenuItem[] = all.map((x, i) => ({
    id: x,
    label: tabLabel(x),
    icon: PANEL_ICON[x],
    shortcut: panelShortcut(x),
    separatorBefore: i > 0 && WORKSPACE_TABS.includes(all[i - 1]) && !WORKSPACE_TABS.includes(x),
    onSelect: () => open(x),
  }));

  return (
    <aside className="panel" aria-label="Panel">
      <ResizeHandle
        edge="left"
        className="panel__resize"
        cssVar="--panel-w"
        label={t("panel.resize")}
        value={shell.panelWidth}
        min={PANEL_MIN}
        max={shell.panelMax}
        defaultValue={PANEL_DEFAULT}
        onCommit={(w) => app.setUi({ panelWidth: w })}
      />
      <div className="panel__head" data-tauri-drag-region>
        <div className="panel__tabs" role="tablist" aria-label={t("panel.tabs")}>
          {tabs.map((x) => (
            <div key={x} className={`panel__tab ${x === tab ? "is-active" : ""}`}>
              <button className="panel__tab-btn" role="tab" aria-selected={x === tab} onClick={() => open(x)}>
                <Icon name={PANEL_ICON[x]} size={14} />
                <span>{tabLabel(x)}</span>
              </button>
              <button className="panel__tab-close" aria-label={t("panel.closeTab", { name: tabLabel(x) })} onClick={() => close(x)}>
                <Icon name="x" size={12} />
              </button>
            </div>
          ))}
        </div>
        <Menu label={t("panel.newTab")} className="panel__menu" items={newTabItems} trigger={(p) => <IconButton icon="plus" size="sm" label={t("panel.newTab")} {...p} />} />
        <span className="grow" data-tauri-drag-region />
        {!dockState(ui).detailsShown && <DockToggles details={false} />}
      </div>
      <div className="panel__body">
        {!tab ? (
          <div className="panel-launcher">
            <div className="panel-launcher__list">
              {panelTabs(ui.mode).map((x) => (
                <button key={x} className="panel-launcher__row" onClick={() => open(x)}>
                  <Icon name={PANEL_ICON[x]} />
                  <span className="panel-launcher__label">{tabLabel(x)}</span>
                  {panelShortcut(x) && <kbd className="panel-launcher__kbd">{panelShortcut(x)}</kbd>}
                </button>
              ))}
            </div>
          </div>
        ) : !sid ? (
          <EmptyState icon={PANEL_ICON[tab]} title={t("panel.noSessionTitle")} hint={t("panel.noSession")} />
        ) : tab === "artifact" ? (
          <ArtifactPanel sessionId={sid} />
        ) : tab === "browser" ? (
          <BrowserPanel key={sid} sessionId={sid} />
        ) : tab === "changes" ? (
          <ChangesPanel sessionId={sid} />
        ) : tab === "files" ? (
          <FilesPanel sessionId={sid} />
        ) : tab === "sidechat" ? (
          <SideChatPanel key={sid} sessionId={sid} />
        ) : tab === "tasks" ? (
          <TasksPanel sessionId={sid} />
        ) : (
          <TerminalPanel sessionId={sid} />
        )}
      </div>
    </aside>
  );
}
