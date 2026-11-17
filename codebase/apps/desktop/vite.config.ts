import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Same setup as prototype 37 (D57): Vite + React, fixed dev port for `tauri dev`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", chunkSizeWarningLimit: 4000 },
  test: {
    include: ["test/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
