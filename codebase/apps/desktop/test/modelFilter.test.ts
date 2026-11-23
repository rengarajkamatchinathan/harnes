import { describe, expect, it } from "vitest";
import { chatModels, isChatModel } from "../src/state/modelFilter";

describe("chat model filter", () => {
  it("hides special-purpose models that aren't chat models", () => {
    for (const name of ["Gemini 2.5 Computer Use Preview 10-2025", "Deep Research Max Preview (Apr-21-2026)", "Antigravity Agent Preview", "Gemini 2.5 Flash Preview TTS", "Gemini 2.5 Flash Native Audio", "Gemini 2.5 Flash Image Preview", "Imagen 4", "Veo 3", "Gemini Robotics-ER", "Gemini Embedding 001"])
      expect(isChatModel({ id: `google/x`, name })).toBe(false);
  });
  it("keeps normal chat models", () => {
    for (const name of ["Gemini 2.5 Flash-Lite", "Gemini 2.5 Pro", "gpt-5.2-atg (gpt-5.2)", "Claude Sonnet 4.5", "Genie Test", "Llama 3.2"])
      expect(isChatModel({ id: "p/m", name })).toBe(true);
  });
  it("needs a ready provider", () => {
    const ms = [
      { id: "g/a", name: "Gemini 2.5 Pro", provider: "google" },
      { id: "g/b", name: "Deep Research Max Preview", provider: "google" },
      { id: "o/c", name: "GPT", provider: "openai" },
    ];
    expect(chatModels(ms, new Set(["google"])).map((m) => m.id)).toEqual(["g/a"]);
  });
});
