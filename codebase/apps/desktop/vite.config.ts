import { cpSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
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
        this.warn("packages/artifact-runtime/dist is missing: React artifacts won't render. Run `bun run build:runtime` from codebase/.");
        return;
      }
      cpSync(runtimeDist, `${out}/artifact-runtime`, { recursive: true });
    },
  };
}

// Same setup as prototype 37 (D57): Vite + React, fixed dev port for `tauri dev`.
export default defineConfig({
  plugins: [react(), artifactRuntime()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", chunkSizeWarningLimit: 4000 },
  test: {
    include: ["test/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
