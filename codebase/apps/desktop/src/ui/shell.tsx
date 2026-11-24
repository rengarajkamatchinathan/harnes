// Window-size–driven layout state for the app frame (05 §1).
//
// Wide windows show the sidebar inline (collapsible, persisted). When the window is narrow, or
// an open right panel would squeeze the conversation below a readable width, the sidebar turns
// into an overlay drawer instead, and below `NARROW` the right panel covers the main column.
import { createContext, useContext, useSyncExternalStore } from "react";

/** Below this width the sidebar is a drawer and the right panel goes full width. */
export const NARROW = 860;
/** The conversation column never gets narrower than this while the sidebar is inline. */
export const MIN_MAIN = 480;
export const SIDEBAR_MIN = 220;
export const SIDEBAR_MAX = 420;
export const SIDEBAR_DEFAULT = 320;
export const PANEL_MIN = 320;
export const PANEL_DEFAULT = 600;

export interface ShellState {
  width: number;
  /** Sidebar is an overlay drawer (not part of the grid). */
  overlay: boolean;
  /** Right panel covers the main column. */
  narrow: boolean;
  drawerOpen: boolean;
  setDrawer: (open: boolean) => void;
  /** The sidebar is not visible (collapsed inline, or drawer closed). */
  sidebarHidden: boolean;
  /** The right panel's width as shown, and how wide each column may be dragged right now. */
  panelWidth: number;
  panelMax: number;
  sidebarMax: number;
}

export const ShellContext = createContext<ShellState>({ width: 1280, overlay: false, narrow: false, drawerOpen: false, setDrawer: () => {}, sidebarHidden: false, panelWidth: 600, panelMax: 600, sidebarMax: SIDEBAR_MAX });
export const useShell = () => useContext(ShellContext);

const subscribe = (cb: () => void) => {
  window.addEventListener("resize", cb);
  return () => window.removeEventListener("resize", cb);
};

export function useWindowWidth(): number {
  return useSyncExternalStore(
    subscribe,
    () => window.innerWidth,
    () => 1280,
  );
}

/** Pure layout decision (for tests). */
/** Width of the session details column. */
export const DETAILS_W = 300;

export function layoutFor(width: number, o: { sidebarWidth: number; panelOpen: boolean; panelWidth: number; detailsOpen?: boolean }) {
  const narrow = width < NARROW;
  const details = !narrow && o.detailsOpen ? DETAILS_W : 0;
  const overlay = narrow || ((o.panelOpen || details > 0) && width - (o.panelOpen ? o.panelWidth : 0) - details - o.sidebarWidth < MIN_MAIN);
  // Keep the panel from eating the conversation: at most width − sidebar − details − MIN_MAIN (min 320).
  const room = width - (overlay ? 0 : o.sidebarWidth) - details - MIN_MAIN;
  const panelMax = Math.max(PANEL_MIN, room);
  const panelWidth = narrow ? width : Math.min(Math.max(PANEL_MIN, o.panelWidth), panelMax);
  // Widening the sidebar must not squeeze the conversation either (nor flip it into a drawer).
  const sidebarMax = Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, width - (o.panelOpen ? panelWidth : 0) - details - MIN_MAIN));
  return { narrow, overlay, panelWidth, panelMax, sidebarMax };
}
