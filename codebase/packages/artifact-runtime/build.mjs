// Builds the artifact runtime into dist/ (served by the desktop shell under
// `genie-artifact://…/_genie/runtime/`, 05 §8):
//
//   runtime.js         the in-sandbox loader (esbuild-wasm JS API included)
//   esbuild-wasm.js    the esbuild wasm binary, gzip + base64 (the CSP forbids fetch)
//   libs/<name>.js     each bundled library, registering into globalThis.__genie_modules
//   libs/katex.css     KaTeX styles (+ libs/fonts/*)
//   libs/tailwind.js   Tailwind CSS v4 browser build (utility classes generated in the frame)
//   manifest.json      file list + versions (checked by the desktop shell tests)
//
// No network at run time: everything the artifact may use is here.
import { build } from "esbuild";
import { mkdirSync, readFileSync, rmSync, writeFileSync, copyFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "dist");
const require = createRequire(import.meta.url);
rmSync(dist, { recursive: true, force: true });
mkdirSync(join(dist, "libs"), { recursive: true });

const REGISTRY = "__genie_modules";
const define = { "process.env.NODE_ENV": '"production"', global: "globalThis" };
const common = { bundle: true, format: "iife", minify: true, target: "es2020", platform: "browser", define, logLevel: "warning", legalComments: "eof" };

const wrap = `const __wrap = (ns) => Object.assign({}, ns, { __esModule: true, default: ns && "default" in ns ? ns.default : ns });
const __reg = (globalThis.${REGISTRY} ??= {});`;

// Libraries other than React use the one React instance from libs/react.js.
const shared = {
  name: "genie-shared-react",
  setup(b) {
    b.onResolve({ filter: /^(react|react-dom)(\/.*)?$/ }, (a) => ({ path: a.path, namespace: "genie-shared" }));
    b.onLoad({ filter: /.*/, namespace: "genie-shared" }, (a) => ({
      contents: `const m = globalThis.${REGISTRY} && globalThis.${REGISTRY}[${JSON.stringify(a.path)}];
if (!m) throw new Error("React must load before ${a.path} users");
module.exports = m.default;`,
      loader: "js",
    }));
  },
};

async function lib(file, entry, opts = {}) {
  await build({
    ...common,
    stdin: { contents: `${wrap}\n${entry}`, resolveDir: here, loader: "js" },
    outfile: join(dist, "libs", `${file}.js`),
    plugins: opts.shareReact === false ? [] : [shared],
  });
}

const reg = (spec, ident) => `__reg[${JSON.stringify(spec)}] = __wrap(${ident});`;

await lib(
  "react",
  `import * as react from "react";
import * as jsx from "react/jsx-runtime";
import * as jsxDev from "react/jsx-dev-runtime";
import * as dom from "react-dom";
import * as client from "react-dom/client";
${reg("react", "react")}
${reg("react/jsx-runtime", "jsx")}
${reg("react/jsx-dev-runtime", "jsxDev")}
${reg("react-dom", "dom")}
${reg("react-dom/client", "client")}`,
  { shareReact: false },
);

for (const pkg of ["lucide-react", "recharts", "d3", "lodash", "papaparse", "mathjs", "three", "mermaid", "katex"]) {
  await lib(pkg, `import * as ns from ${JSON.stringify(pkg)};\n${reg(pkg, "ns")}`);
}

await build({
  entryPoints: [require.resolve("katex/dist/katex.css")],
  bundle: true,
  minify: true,
  outfile: join(dist, "libs", "katex.css"),
  loader: { ".woff2": "file", ".woff": "file", ".ttf": "file" },
  assetNames: "fonts/[name]",
  logLevel: "warning",
});

// Direct dependencies are linked under node_modules/ (some packages don't export package.json).
const pkgDir = (p) => join(here, "node_modules", p);
copyFileSync(join(pkgDir("@tailwindcss/browser"), "dist", "index.global.js"), join(dist, "libs", "tailwind.js"));

await build({ ...common, entryPoints: [join(here, "src", "runtime.ts")], outfile: join(dist, "runtime.js") });

const wasmPath = join(pkgDir("esbuild-wasm"), "esbuild.wasm");
const gz = gzipSync(readFileSync(wasmPath), { level: 9 });
writeFileSync(join(dist, "esbuild-wasm.js"), `globalThis.__GENIE_ESBUILD_WASM__=${JSON.stringify(gz.toString("base64"))};\n`);

const version = (p) => JSON.parse(readFileSync(join(pkgDir(p), "package.json"), "utf8")).version;
const libs = ["react", "react-dom", "lucide-react", "recharts", "d3", "lodash", "papaparse", "mathjs", "three", "mermaid", "katex", "@tailwindcss/browser", "esbuild-wasm"];
writeFileSync(join(dist, "manifest.json"), JSON.stringify({ runtime: "runtime.js", libraries: Object.fromEntries(libs.map((l) => [l, version(l)])) }, null, 2));
console.log("artifact runtime built in", dist);
