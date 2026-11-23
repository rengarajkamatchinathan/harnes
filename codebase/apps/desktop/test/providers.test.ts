import { describe, expect, it } from "vitest";
import { cleanKey, parseAzureEndpoint } from "../src/ui/settings/ProvidersSection";

describe("parseAzureEndpoint", () => {
  it("keeps a bare resource address", () => {
    expect(parseAzureEndpoint("https://res.openai.azure.com/")).toEqual({ base: "https://res.openai.azure.com", deployment: undefined });
    expect(parseAzureEndpoint("res.services.ai.azure.com")).toEqual({ base: "https://res.services.ai.azure.com", deployment: undefined });
  });

  it("splits the portal's Target URI into endpoint and deployment", () => {
    expect(parseAzureEndpoint("https://res.cognitiveservices.azure.com/openai/deployments/gpt-4o-prod/chat/completions?api-version=2025-01-01-preview")).toEqual({
      base: "https://res.cognitiveservices.azure.com",
      deployment: "gpt-4o-prod",
    });
  });

  it("drops inference and project paths", () => {
    expect(parseAzureEndpoint("https://res.services.ai.azure.com/models")?.base).toBe("https://res.services.ai.azure.com");
    expect(parseAzureEndpoint("https://res.services.ai.azure.com/api/projects/my-proj")?.base).toBe("https://res.services.ai.azure.com");
    expect(parseAzureEndpoint("https://res.services.ai.azure.com/anthropic/v1/messages")?.base).toBe("https://res.services.ai.azure.com");
    expect(parseAzureEndpoint("https://res.openai.azure.com/openai/v1/")?.base).toBe("https://res.openai.azure.com");
  });

  it("keeps a gateway prefix in front of the API", () => {
    expect(parseAzureEndpoint("https://apim.azure-api.net/team-a/openai/deployments/x/chat/completions")).toEqual({ base: "https://apim.azure-api.net/team-a", deployment: "x" });
  });

  it("rejects things that aren't addresses", () => {
    expect(parseAzureEndpoint("")).toBeNull();
    expect(parseAzureEndpoint("my key")).toBeNull();
    expect(parseAzureEndpoint("ftp://res.openai.azure.com")).toBeNull();
  });
});

describe("cleanKey", () => {
  it("strips what comes along with a pasted key", () => {
    expect(cleanKey("  sk-abc\n")).toBe("sk-abc");
    expect(cleanKey('"sk-abc"')).toBe("sk-abc");
    expect(cleanKey("Bearer sk-abc")).toBe("sk-abc");
  });
});
