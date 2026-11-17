// The pure part of the React artifact runtime (05 §8, D28, ticket 39), shared by the
// in-sandbox loader (`runtime.ts`) and the tests: the bundled library table, the esbuild
// transform options, import discovery and the module evaluator.
//
// An artifact is a single default-export React component. esbuild-wasm transpiles it (TSX,
// automatic JSX runtime) to CommonJS inside the sandbox; every `require()` must name a
// bundled library, otherwise the artifact fails with a clear "Unknown import" error.

/** One bundled library: the scheme file that registers it, plus an optional stylesheet. */
export interface Lib {
  /** `libs/<file>.js` under `/_genie/runtime/`. */
  file: string;
  css?: string;
}

/**
 * Import specifier → bundled library. Must match `ARTIFACT_LIBRARIES` in
 * `genie-core/src/runtime/artifacts.rs` (what the base prompt advertises): React + ReactDOM,
 * Tailwind CSS, lucide-react, Recharts, d3, lodash, papaparse, mathjs, three.js, Mermaid, KaTeX.
 * Tailwind has no import: its utility classes are always available.
 */
export const LIBRARIES: Record<string, Lib> = {
  react: { file: "react" },
  "react/jsx-runtime": { file: "react" },
  "react/jsx-dev-runtime": { file: "react" },
  "react-dom": { file: "react" },
  "react-dom/client": { file: "react" },
  "lucide-react": { file: "lucide-react" },
  recharts: { file: "recharts" },
  d3: { file: "d3" },
  lodash: { file: "lodash" },
  papaparse: { file: "papaparse" },
  mathjs: { file: "mathjs" },
  three: { file: "three" },
  mermaid: { file: "mermaid" },
  katex: { file: "katex", css: "katex" },
};

/** Stylesheet imports that are satisfied by a bundled library's CSS. */
export const STYLESHEETS: Record<string, string> = {
  "katex/dist/katex.min.css": "katex",
  "katex/dist/katex.css": "katex",
};

/** The names the prompt advertises, for error messages. */
export const ADVERTISED = "react, react-dom, lucide-react, recharts, d3, lodash, papaparse, mathjs, three, mermaid, katex (Tailwind classes need no import)";

/** The global registry the library files fill: specifier → module namespace. */
export const REGISTRY = "__genie_modules";

export class ArtifactError extends Error {
  constructor(
    message: string,
    readonly kind: "compile" | "import" | "export" | "runtime",
  ) {
    super(message);
    this.name = "ArtifactError";
  }
}

/** esbuild `transform` options for an artifact. */
export const TRANSFORM_OPTIONS = {
  loader: "tsx",
  format: "cjs",
  jsx: "automatic",
  target: "es2020",
  sourcefile: "artifact.jsx",
  sourcemap: "inline",
} as const;

export interface Transformer {
  transform(input: string, options: typeof TRANSFORM_OPTIONS): Promise<{ code: string }>;
}

/** Compile; esbuild errors become an `ArtifactError` with the first message and location. */
export async function compile(esbuild: Transformer, source: string): Promise<string> {
  try {
    return (await esbuild.transform(source, TRANSFORM_OPTIONS)).code;
  } catch (e) {
    const errs = (e as { errors?: { text: string; location?: { line: number; column: number; lineText?: string } | null }[] }).errors;
    if (errs?.length) {
      const first = errs[0];
      const at = first.location ? ` (line ${first.location.line}, column ${first.location.column + 1})` : "";
      throw new ArtifactError(`Couldn't compile the artifact: ${first.text}${at}`, "compile");
    }
    throw new ArtifactError(`Couldn't compile the artifact: ${String((e as Error)?.message ?? e)}`, "compile");
  }
}

/** Every `require("…")` in compiled CommonJS output, in order, without duplicates. */
export function findRequires(code: string): string[] {
  const out = new Set<string>();
  // esbuild prints import specifiers as double-quoted string literals.
  for (const m of code.matchAll(/\brequire\("((?:[^"\\]|\\.)*)"\)/g)) out.add(JSON.parse(`"${m[1]}"`) as string);
  return [...out];
}

/** The library files and stylesheets an artifact needs; unknown imports throw. */
export function plan(requires: string[]): { files: string[]; css: string[] } {
  const files = new Set<string>(["react"]);
  const css = new Set<string>();
  const unknown: string[] = [];
  for (const r of requires) {
    const lib = LIBRARIES[r];
    if (lib) {
      files.add(lib.file);
      if (lib.css) css.add(lib.css);
    } else if (STYLESHEETS[r]) {
      css.add(STYLESHEETS[r]);
    } else if (/\.css$/i.test(r)) {
      // Other stylesheets have nothing to load; Tailwind classes are always present.
      continue;
    } else {
      unknown.push(r);
    }
  }
  if (unknown.length) {
    const list = unknown.map((u) => `"${u}"`).join(", ");
    throw new ArtifactError(`Unknown import ${list}. Artifacts run offline and can only import: ${ADVERTISED}.`, "import");
  }
  return { files: [...files], css: [...css] };
}

type Registry = Record<string, unknown>;

/** Evaluate compiled CommonJS with a `require` limited to the registry; returns the component. */
export function evaluate(code: string, registry: Registry): unknown {
  const require = (name: string) => {
    if (STYLESHEETS[name] || /\.css$/i.test(name)) return {};
    if (!(name in registry)) throw new ArtifactError(`Unknown import "${name}". Artifacts can only import: ${ADVERTISED}.`, "import");
    return registry[name];
  };
  const module: { exports: Record<string, unknown> } = { exports: {} };
  // 'unsafe-eval' is allowed by the artifact CSP (05 §8); the code runs in the opaque origin.
  new Function("require", "module", "exports", code)(require, module, module.exports);
  const exp = module.exports;
  const component = (exp && typeof exp === "object" && "default" in exp ? exp.default : exp) as unknown;
  if (typeof component !== "function" && !(component && typeof component === "object" && "$$typeof" in component)) {
    throw new ArtifactError("The artifact must default-export a React component (export default function App() { … }).", "export");
  }
  return component;
}
