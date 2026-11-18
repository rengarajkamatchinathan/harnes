import { describe, expect, it } from "vitest";
import { layoutFor, MIN_MAIN, NARROW } from "../src/ui/shell";

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
});
