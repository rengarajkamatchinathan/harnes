// Lint-as-test for 01 §1.3 / §2.1 rule 8: `@tauri-apps/*` only inside src/platform/tauri/,
// and the app reaches the host only through the Platform adapter.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    return statSync(p).isDirectory() ? files(p) : /\.(ts|tsx)$/.test(n) ? [p] : [];
  });
}

describe("platform boundary", () => {
  const all = files(SRC).map((p) => ({ rel: relative(SRC, p).replace(/\\/g, "/"), text: readFileSync(p, "utf8") }));

  it("imports @tauri-apps only in src/platform/tauri/", () => {
    const bad = all.filter((f) => !f.rel.startsWith("platform/tauri/") && /from\s+["']@tauri-apps\//.test(f.text)).map((f) => f.rel);
    expect(bad).toEqual([]);
  });

  it("touches Tauri globals only in src/platform/", () => {
    const bad = all.filter((f) => !f.rel.startsWith("platform/") && /__TAURI|window\.ipc/.test(f.text)).map((f) => f.rel);
    expect(bad).toEqual([]);
  });

  it("does not import the platform implementations directly", () => {
    const bad = all.filter((f) => !f.rel.startsWith("platform/") && /from\s+["'][./]*platform\/(tauri|mock)["']/.test(f.text)).map((f) => f.rel);
    expect(bad).toEqual([]);
  });

  it("uses only the generated protocol package for wire types", () => {
    const bad = all.filter((f) => /from\s+["'].*protocol-ts\/src/.test(f.text)).map((f) => f.rel);
    expect(bad).toEqual([]);
  });
});
