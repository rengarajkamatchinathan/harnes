import type { Platform } from "./types";

export * from "./types";

/** Tauri inside the app window; the in-memory mock engine in a plain browser (dev/tests). */
export async function detectPlatform(): Promise<Platform> {
  const w = globalThis as { __TAURI_INTERNALS__?: unknown; location?: Location };
  if (w.__TAURI_INTERNALS__) {
    const { createTauriPlatform } = await import("./tauri");
    return createTauriPlatform();
  }
  const { createMockPlatform } = await import("./mock");
  return createMockPlatform({ firstRun: w.location?.search.includes("firstrun") ?? false });
}
