// Claude-Desktop-style shell: greeting text and title-bar back/forward history.
import { describe, expect, it } from "vitest";
import { createMockPlatform } from "../src/platform/mock";
import { App } from "../src/state/app";
import { greeting } from "../src/ui/chat/ChatView";
import { initials } from "../src/ui/Sidebar";

const at = (h: number) => new Date(2026, 10, 17, h, 0, 0);

describe("greeting", () => {
  it("follows the time of day and the chosen name", () => {
    expect(greeting("Sam", at(9))).toBe("Good morning, Sam");
    expect(greeting("Sam", at(14))).toBe("Good afternoon, Sam");
    expect(greeting("", at(19))).toBe("Good evening");
    expect(greeting("Sam", at(2))).toBe("Up late, Sam?");
    expect(greeting(" ", at(23))).toBe("Up late?");
  });
  it("makes avatar initials", () => {
    expect(initials("Ada Lovelace")).toBe("AL");
    expect(initials("ada")).toBe("A");
    expect(initials("")).toBe("G");
  });
});

describe("back/forward", () => {
  it("steps through places and skips the settings modal", async () => {
    const app = new App(createMockPlatform({ tickMs: 0 }));
    await app.start();
    expect(app.canGoBack).toBe(false);
    app.setUi({ mode: "code", activeSession: null });
    app.setUi({ view: "settings" });
    app.setUi({ view: "main" });
    expect(app.canGoBack).toBe(true);
    app.goBack();
    expect(app.ui.mode).toBe("chat");
    expect(app.canGoForward).toBe(true);
    app.goForward();
    expect(app.ui.mode).toBe("code");
    expect(app.canGoForward).toBe(false);
  });
});
