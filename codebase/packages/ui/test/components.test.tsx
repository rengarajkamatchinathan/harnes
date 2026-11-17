import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Banner, Button, Dialog, Field, ICON_NAMES, Icon, IconButton, Ring, Select, Switch, Tabs } from "../src";

describe("components", () => {
  it("renders accessible controls", () => {
    expect(renderToStaticMarkup(<Button variant="primary">Send</Button>)).toContain('class="g-btn g-btn--primary"');
    expect(renderToStaticMarkup(<IconButton icon="copy" label="Copy" />)).toContain('aria-label="Copy"');
    const sw = renderToStaticMarkup(<Switch checked onChange={() => {}} label="On" />);
    expect(sw).toContain('role="switch"');
    expect(sw).toContain('aria-checked="true"');
    const tabs = renderToStaticMarkup(<Tabs label="Panel" value="b" onChange={() => {}} tabs={[{ id: "a", label: "A" }, { id: "b", label: "B" }]} />);
    expect(tabs).toContain('role="tablist"');
    expect(tabs).toMatch(/aria-selected="true"[^>]*data-tab="b"|data-tab="b"[^>]*aria-selected="true"/);
    expect(renderToStaticMarkup(<Banner tone="danger">x</Banner>)).toContain('role="alert"');
    expect(renderToStaticMarkup(<Ring value={0.5} label="Context 50%" />)).toContain('aria-label="Context 50%"');
  });

  it("groups select options and shows locks", () => {
    const html = renderToStaticMarkup(
      <Select value="a" options={[{ value: "a", label: "A", group: "P1" }, { value: "b", label: "B", group: "P2" }]} />,
    );
    expect(html).toContain('<optgroup label="P1">');
    const f = renderToStaticMarkup(
      <Field label="Theme" locked="Managed by Acme" error="bad">
        <input />
      </Field>,
    );
    expect(f).toContain("Managed by Acme");
    expect(f).toContain('role="alert"');
  });

  it("renders a closed dialog as nothing and every icon", () => {
    expect(renderToStaticMarkup(<Dialog open={false} title="t" onClose={() => {}}>x</Dialog>)).toBe("");
    for (const n of ICON_NAMES) expect(renderToStaticMarkup(<Icon name={n} />)).toContain("<path");
  });
});
