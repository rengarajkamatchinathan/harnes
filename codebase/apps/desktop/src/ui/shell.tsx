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
export const SIDEBAR_DEFAULT = 280;

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
}

export const ShellContext = createContext<ShellState>({ width: 1280, overlay: false, narrow: false, drawerOpen: false, setDrawer: () => {}, sidebarHidden: false });
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
export function layoutFor(width: number, o: { sidebarWidth: number; panelOpen: boolean; panelWidth: number }) {
  const narrow = width < NARROW;
  const overlay = narrow || (o.panelOpen && width - o.panelWidth - o.sidebarWidth < MIN_MAIN);
  // Keep the panel from eating the conversation: at most width − sidebar − MIN_MAIN (min 320).
  const room = width - (overlay ? 0 : o.sidebarWidth) - MIN_MAIN;
  const panelWidth = narrow ? width : Math.max(320, Math.min(o.panelWidth, room));
  return { narrow, overlay, panelWidth };
}
