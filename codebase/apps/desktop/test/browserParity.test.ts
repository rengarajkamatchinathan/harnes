// Ticket 47 phases 2–4: pure helpers of the dev-server card, file previews and Element select.
import { describe, expect, it } from "vitest";
import { PREVIEW_PATH, renderSettled } from "../src/markdown/markdown";
import { elementMessage } from "../src/ui/panel/BrowserPanel";
import { summarizeTool } from "../src/ui/chat/toolInfo";

describe("file previews from chat", () => {
  it("matches local HTML, PDF, video and SVG paths only", () => {
    for (const p of ["index.html", "out/report.pdf", "./docs/a b.htm".replace(" ", "_"), "/abs/demo.mp4", "logo.SVG"]) expect(PREVIEW_PATH.test(p), p).toBe(true);
    for (const p of ["https://x.com/a.html", "file:///a.html", "readme.md", "a.html b", "javascript:x.html"]) expect(PREVIEW_PATH.test(p), p).toBe(false);
  });

  it("renders matching inline code and relative links as preview targets", () => {
    const html = renderSettled("Open `dist/index.html` or [the report](out/report.pdf); `src/main.ts` stays code.");
    expect(html).toContain('<code class="preview-path" role="button" tabindex="0" data-preview="dist/index.html"');
    expect(html).toContain('<a href="#" class="preview-path" data-preview="out/report.pdf"');
    expect(html).toContain("<code>src/main.ts</code>");
  });

  it("escapes paths", () => {
    const html = renderSettled('`a"<b>.html`');
    expect(html).not.toContain("<b>");
  });
});

describe("element select", () => {
  it("formats the picked element for the composer", () => {
    const m = elementMessage({ url: "http://localhost:5173/", selector: "#save", text: "Save", html: '<button id="save">Save</button>' });
    expect(m).toBe('Selected element on http://localhost:5173/\nSelector: `#save`\nText: "Save"\n```html\n<button id="save">Save</button>\n```');
    const long = elementMessage({ url: "u", selector: "p", text: "x".repeat(300), html: "<p></p>" });
    expect(long).toContain(`Text: "${"x".repeat(199)}…"`);
  });
});

describe("dev server rows", () => {
  it("summarise preview tools", () => {
    expect(summarizeTool("preview_start", { name: "web", runtimeExecutable: "npm", runtimeArgs: ["run", "dev"] }, "ok").title).toBe("Started dev server `web` · `npm run dev`");
    expect(summarizeTool("preview_start", { name: "web" }, null).title).toBe("Starting dev server `web`");
    expect(summarizeTool("preview_start", { name: "web" }, "error").title).toBe("Couldn't start `web`");
    expect(summarizeTool("preview_stop", {}, "ok").title).toBe("Stopped dev servers");
  });
});
