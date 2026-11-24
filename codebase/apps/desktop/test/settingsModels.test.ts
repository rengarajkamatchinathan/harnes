// Settings → Models: `hiddenModels` keeps models out of the pickers but in the Settings list;
// admin-locked keys are reported by `config.get` and refused by `config.set`.
import { describe, expect, it } from "vitest";
import { errorKind } from "../src/platform";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { stepZoom, ZOOM_STEPS } from "../src/ui/settings/schema";

const until = async (f: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

async function started(lockedKeys?: string[]) {
  const app = new App(createMockPlatform({ tickMs: 0, reply: () => "hello", lockedKeys }));
  await app.start();
  await until(() => app.usableModels.length > 1);
  return app;
}

describe("Settings → Models", () => {
  it("hides a model from the pickers but keeps it in the Settings list", async () => {
    const app = await started();
    const all = app.usableModels.map((m) => m.id);
    const target = all[1];
    await app.client.config.set({ key: "hiddenModels", value: [target], scope: "user", cwd: null });
    await app.refreshLists();
    expect(app.hiddenModels.has(target)).toBe(true);
    expect(app.usableModels.map((m) => m.id)).not.toContain(target);
    expect(app.chatReadyModels.map((m) => m.id)).toContain(target);
    // A hidden default model is not used for new sessions.
    await app.client.config.set({ key: "model", value: target, scope: "user", cwd: null });
    await app.refreshLists();
    expect(app.preferredModel()).not.toBe(target);
  });

  it("reports admin-locked keys and refuses to change them", async () => {
    const app = await started(["hiddenModels"]);
    const r = await app.client.config.get({ keys: ["hiddenModels", "model"], cwd: null, scope: null });
    expect(r.locked).toEqual(["hiddenModels"]);
    const failure = await app.client.config.set({ key: "hiddenModels", value: ["x"], scope: "user", cwd: null }).then(
      () => null,
      (e: unknown) => e,
    );
    expect(errorKind(failure)).toBe("locked");
  });
});

describe("Settings → Appearance → Zoom", () => {
  it("steps through browser-like zoom levels and stops at the ends", () => {
    expect(stepZoom(1, 1)).toBe(1.1);
    expect(stepZoom(1, -1)).toBe(0.9);
    expect(stepZoom(ZOOM_STEPS[ZOOM_STEPS.length - 1], 1)).toBe(ZOOM_STEPS[ZOOM_STEPS.length - 1]);
    expect(stepZoom(ZOOM_STEPS[0], -1)).toBe(ZOOM_STEPS[0]);
  });
});
