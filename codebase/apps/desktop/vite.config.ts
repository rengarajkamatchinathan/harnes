import { cpSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { loadEnv } from "vite";
import { defineConfig, type Plugin } from "vitest/config";
import react from "@vitejs/plugin-react";

const runtimeDist = fileURLToPath(new URL("../../packages/artifact-runtime/dist", import.meta.url));

/**
 * The React artifact runtime (05 §8) ships inside the frontend bundle as
 * `dist/artifact-runtime/`, so release builds serve it from the embedded assets on the
 * genie-artifact scheme. It is never loaded by the app page itself.
 */
function artifactRuntime(): Plugin {
  return {
    name: "genie-artifact-runtime",
    apply: "build",
    writeBundle(options) {
      const out = options.dir ?? fileURLToPath(new URL("./dist", import.meta.url));
      if (!existsSync(runtimeDist)) {
        this.warn("packages/artifact-runtime/dist is missing: React artifacts won't render. Run `pnpm --filter @genie/artifact-runtime build`.");
        return;
      }
      cpSync(runtimeDist, `${out}/artifact-runtime`, { recursive: true });
    },
  };
}

/**
 * Dev-only Azure OpenAI bridge for the browser preview: with AZURE_API_KEY etc. in
 * `.env.local`, the mock engine gets a real model. The dev server proxies `/azure-openai/*`
 * to the endpoint and adds the key itself, so the key never reaches the page or the bundle.
 */
function azureDev(command: string, mode: string) {
  const env = loadEnv(mode, fileURLToPath(new URL(".", import.meta.url)), "AZURE_");
  const endpoint = env.AZURE_OPENAI_ENDPOINT?.replace(/\/+$/, "");
  const on = command === "serve" && mode !== "test" && !!env.AZURE_API_KEY && !!endpoint;
  const client = on ? { deployment: env.AZURE_OPENAI_DEPLOYMENT_ID || "gpt-5.2-atg", apiVersion: env.AZURE_OPENAI_API_VERSION || "2024-04-01-preview" } : null;
  const proxy = on
    ? { "/azure-openai": { target: endpoint!, changeOrigin: true, rewrite: (p: string) => p.replace(/^\/azure-openai/, ""), headers: { "api-key": env.AZURE_API_KEY } } }
    : undefined;
  return { client, proxy };
}

// Same setup as prototype 37 (D57): Vite + React, fixed dev port for `tauri dev`.
export default defineConfig(({ command, mode }) => {
  const azure = azureDev(command, mode);
  return {
    plugins: [react(), artifactRuntime()],
    clearScreen: false,
    define: { __GENIE_AZURE__: JSON.stringify(azure.client) },
    server: { port: 5173, strictPort: true, proxy: azure.proxy },
    build: { target: "es2022", chunkSizeWarningLimit: 4000 },
    test: {
      include: ["test/**/*.test.{ts,tsx}"],
      environment: "node",
    },
  };
});
