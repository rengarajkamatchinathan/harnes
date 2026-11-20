// Real model for the browser preview: Azure OpenAI chat completions, streamed through the
// Vite dev-server proxy (`/azure-openai`, see vite.config.ts), which adds the API key.

export interface AzureConfig {
  deployment: string;
  apiVersion: string;
}

export const azureConfig: AzureConfig | null = typeof __GENIE_AZURE__ !== "undefined" ? __GENIE_AZURE__ : null;

export const azureModelId = (c: AzureConfig) => `azure-foundry/${c.deployment}`;

export interface ChatMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

export interface AzureUsage {
  input_tokens: number;
  output_tokens: number;
}

/** Streams a chat completion; calls `onDelta` per text chunk and resolves with usage when known. */
export async function azureStream(cfg: AzureConfig, messages: ChatMessage[], onDelta: (text: string) => void, signal: AbortSignal): Promise<AzureUsage | null> {
  const url = `/azure-openai/deployments/${encodeURIComponent(cfg.deployment)}/chat/completions?api-version=${encodeURIComponent(cfg.apiVersion)}`;
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ messages, stream: true }),
    signal,
  });
  if (!res.ok || !res.body) {
    let detail = `${res.status} ${res.statusText}`;
    try {
      const j = (await res.json()) as { error?: { message?: string } };
      if (j.error?.message) detail = j.error.message;
    } catch {
      /* keep the status line */
    }
    throw new Error(`Azure OpenAI: ${detail}`);
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  let usage: AzureUsage | null = null;
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (!line.startsWith("data:")) continue;
      const data = line.slice(5).trim();
      if (data === "[DONE]") return usage;
      try {
        const j = JSON.parse(data) as {
          choices?: { delta?: { content?: string | null } }[];
          usage?: { prompt_tokens?: number; completion_tokens?: number } | null;
        };
        const text = j.choices?.[0]?.delta?.content;
        if (text) onDelta(text);
        if (j.usage) usage = { input_tokens: j.usage.prompt_tokens ?? 0, output_tokens: j.usage.completion_tokens ?? 0 };
      } catch {
        /* ignore keep-alives and partial lines */
      }
    }
  }
  return usage;
}
