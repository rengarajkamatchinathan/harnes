import { describe, expect, it } from "vitest";
import { testReply } from "../src/platform/mock/testModel";

describe("Genie Test mock model", () => {
  it("lists its keywords on help", () => {
    expect(testReply("help").text).toContain("Genie Test");
  });
  it("picks replies by keyword", () => {
    expect(testReply("show me some code").text).toContain("```rust");
    expect(testReply("a table please").text).toContain("| Model |");
    expect(testReply("math").text).toContain("$$");
  });
  it("adds reasoning for think and fails on error", () => {
    expect(testReply("think about code").thinking).toBeTruthy();
    expect(testReply("trigger an error").error).toBeTruthy();
  });
  it("echoes anything else", () => {
    const r = testReply("hello there");
    expect(r.text).toContain("> hello there");
    expect(r.thinking).toBeUndefined();
    expect(r.error).toBeUndefined();
  });
});
