import { describe, expect, it } from "vitest";
import { DETAILS_W, layoutFor, MIN_MAIN, NARROW, PANEL_MIN, SIDEBAR_MAX, SIDEBAR_MIN } from "../src/ui/shell";

const base = { sidebarWidth: 280, panelOpen: false, panelWidth: 420 };

describe("layoutFor (05 §1 responsive frame)", () => {
  it("keeps the sidebar inline on wide windows", () => {
    expect(layoutFor(1440, base)).toMatchObject({ narrow: false, overlay: false });
    expect(layoutFor(1440, { ...base, panelOpen: true })).toMatchObject({ overlay: false, panelWidth: 420 });
  });

  it("turns the sidebar into a drawer below the narrow breakpoint, with a full-width panel", () => {
    const l = layoutFor(NARROW - 1, { ...base, panelOpen: true });
    expect(l).toMatchObject({ narrow: true, overlay: true, panelWidth: NARROW - 1 });
  });

  it("uses the drawer when an open panel would squeeze the conversation", () => {
    // 1000 − 420 − 280 = 300 < MIN_MAIN
    expect(layoutFor(1000, { ...base, panelOpen: true })).toMatchObject({ narrow: false, overlay: true });
    expect(layoutFor(1000, base).overlay).toBe(false);
  });

  it("caps the panel so the conversation keeps MIN_MAIN, but never below 320", () => {
    const l = layoutFor(1300, { ...base, panelOpen: true, panelWidth: 900 });
    expect(l.panelWidth).toBeLessThanOrEqual(1300 - MIN_MAIN);
    expect(layoutFor(900, { ...base, panelOpen: true, panelWidth: 900 }).panelWidth).toBeGreaterThanOrEqual(320);
  });

  it("counts the details column: drawer when it squeezes the conversation, and the panel shrinks to make room", () => {
    // 1100 − 300 − 280 = 520 ≥ MIN_MAIN; adding a 420 panel leaves 100.
    expect(layoutFor(1100, { ...base, detailsOpen: true }).overlay).toBe(false);
    expect(layoutFor(1100, { ...base, detailsOpen: true, panelOpen: true }).overlay).toBe(true);
    // 1500 − 900 − 300 − 280 < MIN_MAIN → drawer, and the panel caps at 1500 − details − MIN_MAIN.
    const l = layoutFor(1500, { ...base, detailsOpen: true, panelOpen: true, panelWidth: 900 });
    expect(l.overlay).toBe(true);
    expect(l.panelWidth).toBe(1500 - DETAILS_W - MIN_MAIN);
  });

  it("caps dragging at the room the layout really has, so a splitter never goes dead", () => {
    // With the sidebar inline, the panel can be dragged up to what leaves MIN_MAIN for the chat.
    const fit = layoutFor(1440, { ...base, panelOpen: true, panelWidth: 600 });
    expect(fit.overlay).toBe(false);
    expect(fit.panelMax).toBe(1440 - 280 - MIN_MAIN);
    expect(layoutFor(1440, { ...base, panelOpen: true, panelWidth: fit.panelMax }).overlay).toBe(false);
    // A saved width past that is shown clamped (what the handle starts from).
    const l = layoutFor(1440, { ...base, panelOpen: true, panelWidth: 2000 });
    expect(l.panelWidth).toBe(l.panelMax);
    // Widening the sidebar to its max keeps the conversation at MIN_MAIN or more.
    expect(1440 - fit.panelWidth - fit.sidebarMax).toBeGreaterThanOrEqual(MIN_MAIN);
    expect(layoutFor(1440, base).sidebarMax).toBe(SIDEBAR_MAX);
    expect(layoutFor(900, { ...base, panelOpen: true }).sidebarMax).toBe(SIDEBAR_MIN);
    expect(layoutFor(900, { ...base, panelOpen: true }).panelMax).toBeGreaterThanOrEqual(PANEL_MIN);
  });
});
