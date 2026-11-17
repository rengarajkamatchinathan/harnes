// The in-sandbox loader for React artifacts (05 §8, D28). Served as
// `/_genie/runtime/runtime.js` on the genie-artifact scheme and run inside
// `<iframe sandbox="allow-scripts">` (opaque origin, `connect-src 'none'`).
//
// 1. The page's `source.js` set `globalThis.__GENIE_SOURCE__` (the artifact code).
// 2. esbuild-wasm is initialised from `esbuild-wasm.js` (the gzip'd wasm as base64: the CSP
//    forbids fetch, so the module is compiled from bytes; 'wasm-unsafe-eval' allows that).
// 3. The code is transpiled to CommonJS; its imports pick the library files to load
//    (`libs/<name>.js`, each registering into `globalThis.__genie_modules`); unknown imports
//    fail with a clear message.
// 4. The default export renders into `#root` under an error boundary. Compile, import and
//    render errors are shown in the frame and posted to the parent as
//    `runtime_error {message, stack}` (the "Fix with Genie" banner).
import * as esbuild from "esbuild-wasm";
import { ArtifactError, REGISTRY, compile, evaluate, findRequires, plan } from "./compile";

const BASE = "/_genie/runtime/";

type G = typeof globalThis & {
  __GENIE_SOURCE__?: string;
  __GENIE_ESBUILD_WASM__?: string;
  [REGISTRY]?: Record<string, any>;
};
const g = globalThis as G;

function post(message: unknown) {
  try {
    parent.postMessage(message, "*");
  } catch {
    /* no parent */
  }
}

function loadScript(src: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = src;
    s.onload = () => resolve();
    s.onerror = () => reject(new ArtifactError(`Couldn't load ${src}`, "runtime"));
    document.head.appendChild(s);
  });
}

function loadCss(href: string): Promise<void> {
  return new Promise((resolve) => {
    const l = document.createElement("link");
    l.rel = "stylesheet";
    l.href = href;
    l.onload = () => resolve();
    l.onerror = () => resolve();
    document.head.appendChild(l);
  });
}

async function wasmModule(): Promise<WebAssembly.Module> {
  await loadScript(`${BASE}esbuild-wasm.js`);
  const b64 = g.__GENIE_ESBUILD_WASM__;
  if (!b64) throw new ArtifactError("The esbuild runtime is missing.", "runtime");
  const bin = atob(b64);
  const gz = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) gz[i] = bin.charCodeAt(i);
  delete g.__GENIE_ESBUILD_WASM__;
  // `Response` here is a local body reader, not a network request.
  const stream = new Blob([gz]).stream().pipeThrough(new DecompressionStream("gzip"));
  const bytes = await new Response(stream).arrayBuffer();
  return WebAssembly.compile(bytes);
}

function showError(err: { message: string; stack?: string }) {
  const root = document.getElementById("root");
  if (!root) return;
  root.textContent = "";
  const box = document.createElement("div");
  box.className = "genie-error";
  const title = document.createElement("strong");
  title.textContent = "This artifact couldn't run";
  const pre = document.createElement("pre");
  pre.textContent = err.message;
  box.append(title, pre);
  root.append(box);
}

function report(e: unknown) {
  const err = e instanceof Error ? e : new Error(String(e));
  showError(err);
  post({ type: "runtime_error", message: err.message, stack: err.stack ?? "" });
}

async function main() {
  const source = g.__GENIE_SOURCE__ ?? "";
  const registry = (g[REGISTRY] ??= {});
  const [module] = await Promise.all([wasmModule(), loadScript(`${BASE}libs/react.js`), loadScript(`${BASE}libs/tailwind.js`)]);
  await esbuild.initialize({ wasmModule: module, worker: false });
  const code = await compile(esbuild, source);
  const need = plan(findRequires(code));
  await Promise.all([...need.files.filter((f) => f !== "react").map((f) => loadScript(`${BASE}libs/${f}.js`)), ...need.css.map((c) => loadCss(`${BASE}libs/${c}.css`))]);
  const Component = evaluate(code, registry);

  const React = registry.react.default;
  const { createRoot } = registry["react-dom/client"];
  class Boundary extends React.Component {
    state = { error: null as Error | null };
    static getDerivedStateFromError(error: Error) {
      return { error };
    }
    componentDidCatch(error: Error, info: { componentStack?: string }) {
      post({ type: "runtime_error", message: error.message, stack: `${error.stack ?? ""}${info.componentStack ?? ""}` });
    }
    render() {
      const error = (this as unknown as { state: { error: Error | null } }).state.error;
      if (error) return React.createElement("div", { className: "genie-error" }, React.createElement("strong", null, "This artifact crashed"), React.createElement("pre", null, error.message));
      return (this as unknown as { props: { children: unknown } }).props.children;
    }
  }
  const root = createRoot(document.getElementById("root"));
  root.render(React.createElement(Boundary, null, React.createElement(Component)));
}

main().catch(report);
