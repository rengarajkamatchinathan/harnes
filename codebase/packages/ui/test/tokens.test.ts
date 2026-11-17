import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { COLOR_TOKENS, MOTION_TOKENS, SHAPE_TOKENS, TYPE_TOKENS, contrast, parseOklch, resolveTheme, type Oklch } from "../src";

const css = readFileSync(new URL("../src/tokens.css", import.meta.url), "utf8");

function block(selector: string): Map<string, string> {
  const start = css.indexOf(selector);
  const open = css.indexOf("{", start);
  const close = css.indexOf("\n}", open);
  const vars = new Map<string, string>();
  for (const m of css.slice(open + 1, close).matchAll(/(--[\w-]+):\s*([^;]+);/g)) vars.set(m[1], m[2].trim());
  return vars;
}

const base = block(":root {");
const light = block('[data-theme="light"] {');
const dark = block('[data-theme="dark"] {');

function color(vars: Map<string, string>, name: string): Oklch {
  let v = vars.get(name) ?? "";
  for (let i = 0; i < 5 && v.startsWith("var("); i++) v = vars.get(v.slice(4, -1)) ?? "";
  const c = parseOklch(v);
  if (!c) throw new Error(`${name} does not resolve to oklch: ${vars.get(name)}`);
  return c;
}

describe("tokens (05 §6)", () => {
  it("defines every colour token in both themes", () => {
    for (const t of COLOR_TOKENS) {
      expect(light.has(t), `light ${t}`).toBe(true);
      expect(dark.has(t), `dark ${t}`).toBe(true);
    }
  });

  it("defines type, shape and motion tokens", () => {
    for (const t of [...TYPE_TOKENS, ...SHAPE_TOKENS, ...MOTION_TOKENS]) expect(base.has(t), t).toBe(true);
    expect(base.get("--text-base")).toBe("14px");
    expect(base.get("--radius-md")).toBe("8px");
    expect(base.get("--dur-fast")).toBe("120ms");
  });

  it("has 12-step grey and accent scales with the SA-20 placeholder accent", () => {
    for (const vars of [light, dark])
      for (let i = 1; i <= 12; i++) {
        expect(vars.has(`--gray-${i}`)).toBe(true);
        expect(vars.has(`--accent-${i}`)).toBe(true);
      }
    expect(light.get("--accent-9")).toBe("oklch(0.620 0.130 195)");
  });

  it.each([
    ["light", light],
    ["dark", dark],
  ])("meets WCAG AA contrast in %s", (_name, vars) => {
    const pairs: [string, string, number][] = [
      ["--text", "--bg", 4.5],
      ["--text", "--surface-raised", 4.5],
      ["--text-muted", "--bg", 4.5],
      ["--text-muted", "--surface-raised", 4.5],
      ["--accent-fg", "--accent", 4.5],
      ["--danger", "--bg", 4.5],
      ["--success", "--bg", 4.5],
      ["--warning", "--bg", 4.5],
      ["--info", "--bg", 4.5],
      ["--text", "--diff-add", 4.5],
      ["--text", "--diff-del", 4.5],
      ["--text", "--code-bg", 4.5],
      ["--focus-ring", "--bg", 3],
    ];
    for (const [fg, bg, min] of pairs) {
      const r = contrast(color(vars, fg), color(vars, bg));
      expect(r, `${fg} on ${bg} = ${r.toFixed(2)}`).toBeGreaterThanOrEqual(min);
    }
  });

  it("resolves the theme preference", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
  });
});
