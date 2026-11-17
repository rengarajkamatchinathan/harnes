// The React artifact runtime's compile/import/evaluate path (05 §8, D28) with the same
// esbuild-wasm the sandbox uses, plus the built library files evaluated in a fresh VM
// context (as the sandbox loads them: plain scripts registering into a global).
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createContext, runInContext } from "node:vm";
import * as esbuild from "esbuild-wasm";
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ArtifactError, LIBRARIES, compile, evaluate, findRequires, plan } from "../src/compile";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "..", "dist");
const wrap = (ns: Record<string, unknown>) => ({ ...ns, __esModule: true, default: "default" in ns ? ns.default : ns });

const COUNTER = `
import { useState } from "react";
import { Camera } from "lucide-react";
import _ from "lodash";
import "katex/dist/katex.min.css";

type Props = { start?: number };
export default function Counter({ start = 2 }: Props) {
  const [n] = useState(start);
  return <div className="p-4 text-lg">{_.repeat("*", n)}<Camera /></div>;
}
`;

describe("compile and imports", () => {
  it("transpiles TSX to CommonJS and finds the imports", async () => {
    const code = await compile(esbuild, COUNTER);
    expect(code).not.toContain("<div");
    const reqs = findRequires(code);
    expect(reqs).toEqual(expect.arrayContaining(["react", "lucide-react", "lodash", "katex/dist/katex.min.css", "react/jsx-runtime"]));
    const p = plan(reqs);
    expect(p.files.sort()).toEqual(["lodash", "lucide-react", "react"]);
    expect(p.css).toEqual(["katex"]);
  });

  it("rejects imports outside the bundled set with a clear message", async () => {
    const code = await compile(esbuild, `import axios from "axios";\nimport x from "./local";\nexport default () => { axios(x); return null; };`);
    expect(() => plan(findRequires(code))).toThrow(ArtifactError);
    expect(() => plan(findRequires(code))).toThrow(/Unknown import "axios", "\.\/local"\. Artifacts run offline and can only import: react/);
  });

  it("reports compile errors with a location", async () => {
    await expect(compile(esbuild, "export default function () { return <div> }")).rejects.toThrow(/Couldn't compile the artifact: .*\(line 1, column \d+\)/);
  });

  it("evaluates the default export with a registry-only require", async () => {
    const code = await compile(esbuild, `import { useState } from "react";\nexport default function App() { const [v] = useState("hi"); return <b>{v}</b>; }`);
    const registry = { react: wrap(React as never), "react/jsx-runtime": wrap(jsxRuntime as never) };
    const C = evaluate(code, registry) as () => React.ReactElement;
    expect(renderToStaticMarkup(React.createElement(C))).toBe("<b>hi</b>");
  });

  it("requires a default-exported component", async () => {
    const code = await compile(esbuild, `export const x = 1;`);
    expect(() => evaluate(code, {})).toThrow(/must default-export a React component/);
  });

  it("covers every library the base prompt advertises (genie-core ARTIFACT_LIBRARIES)", () => {
    const rs = readFileSync(join(here, "..", "..", "..", "crates", "genie-core", "src", "runtime", "artifacts.rs"), "utf8");
    const m = /ARTIFACT_LIBRARIES: &str = "([^;]+);/.exec(rs);
    expect(m).not.toBeNull();
    const advertised = m![1].replace(/\\\n/g, "").toLowerCase();
    const names: Record<string, string> = { react: "react", reactdom: "react-dom", "lucide-react": "lucide-react", recharts: "recharts", d3: "d3", lodash: "lodash", papaparse: "papaparse", mathjs: "mathjs", "three.js": "three", mermaid: "mermaid", katex: "katex" };
    for (const [word, spec] of Object.entries(names)) {
      expect(advertised).toContain(word);
      expect(LIBRARIES[spec], spec).toBeDefined();
    }
    expect(advertised).toContain("tailwind");
  });
});

describe.skipIf(!existsSync(join(dist, "libs", "react.js")))("built library files", () => {
  function sandbox() {
    const ctx = createContext({ console, setTimeout, clearTimeout, queueMicrotask, MessageChannel, TextEncoder, TextDecoder });
    runInContext("globalThis.window = globalThis; globalThis.self = globalThis;", ctx);
    const load = (f: string) => runInContext(readFileSync(join(dist, "libs", f), "utf8"), ctx, { filename: f });
    const registry = () => runInContext("globalThis.__genie_modules", ctx) as Record<string, any>;
    return { load, registry };
  }

  it("register React once and let other libraries share it", () => {
    const s = sandbox();
    s.load("react.js");
    s.load("lodash.js");
    s.load("lucide-react.js");
    s.load("papaparse.js");
    const r = s.registry();
    for (const k of ["react", "react/jsx-runtime", "react-dom", "react-dom/client", "lodash", "lucide-react", "papaparse"]) expect(r[k], k).toBeDefined();
    expect(r.lodash.default.chunk([1, 2, 3], 2)).toEqual([[1, 2], [3]]);
    expect(typeof r["lucide-react"].Camera).toBe("object"); // forwardRef component
    expect(r.papaparse.default.parse("a,b\n1,2").data[1]).toEqual(["1", "2"]);
  });

  it("ship every library file the table names, the runtime and the wasm", () => {
    for (const lib of new Set(Object.values(LIBRARIES).map((l) => l.file))) expect(existsSync(join(dist, "libs", `${lib}.js`)), lib).toBe(true);
    for (const f of ["runtime.js", "esbuild-wasm.js", "libs/tailwind.js", "libs/katex.css", "manifest.json"]) expect(existsSync(join(dist, f)), f).toBe(true);
  });
});
