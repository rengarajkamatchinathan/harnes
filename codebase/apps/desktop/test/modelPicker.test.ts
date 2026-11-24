import { describe, expect, it } from "vitest";
import type { ModelInfo } from "@genie/protocol";
import { splitModels } from "../src/ui/chat/Composer";

const models = (n: number) => Array.from({ length: n }, (_, i) => ({ id: `m${i + 1}`, name: `M${i + 1}`, provider: "p" }) as unknown as ModelInfo);
const ids = (xs: ModelInfo[]) => xs.map((m) => m.id);

describe("splitModels (model menu: short list + More models)", () => {
  it("lists everything when it fits with one row to spare", () => {
    expect(splitModels(models(5), "m1")).toMatchObject({ more: [] });
    expect(ids(splitModels(models(5), "m1").top)).toEqual(["m1", "m2", "m3", "m4", "m5"]);
  });
  it("puts the rest under More models", () => {
    const r = splitModels(models(7), "m2");
    expect(ids(r.top)).toEqual(["m1", "m2", "m3", "m4"]);
    expect(ids(r.more)).toEqual(["m5", "m6", "m7"]);
  });
  it("keeps the current model in the short list", () => {
    const r = splitModels(models(7), "m6");
    expect(ids(r.top)).toEqual(["m1", "m2", "m3", "m6"]);
    expect(ids(r.more)).toEqual(["m4", "m5", "m7"]);
  });
});

import { shortModelName } from "../src/ui/chat/Composer";
describe("shortModelName", () => {
  it("drops a bracket that repeats the name, and a bare id's provider", () => {
    expect(shortModelName("gpt-5.2-atg (gpt-5.2)")).toBe("gpt-5.2-atg");
    expect(shortModelName("openai/gpt-5.2")).toBe("gpt-5.2");
    expect(shortModelName("Claude Sonnet 4.5")).toBe("Claude Sonnet 4.5");
    expect(shortModelName("Llama 3 (local)")).toBe("Llama 3 (local)");
  });
});
