// "+" menu → Attach MCP resource / MCP prompt (05 §2.3, D30). Resources come from
// `mcp.resources` and are read with `mcp.resource.read` (the daemon stores the content as a
// blob), so they attach like a file. Prompts come from `mcp.prompts`; the argument form
// builds the `/server:prompt key="value"` command in the composer.
import { useEffect, useState } from "react";
import type { McpServerInfo } from "@genie/protocol";
import { Button, Dialog, Input } from "@genie/ui";
import { t } from "../../i18n";
import { useApp } from "../context";

export interface McpResource {
  server: string;
  uri: string;
  name: string;
  description?: string;
  mime?: string;
}

export interface McpPromptArg {
  name: string;
  description?: string;
  required: boolean;
}

export interface McpPrompt {
  server: string;
  name: string;
  description?: string;
  arguments: McpPromptArg[];
}

const str = (v: unknown) => (typeof v === "string" ? v : undefined);

/** `mcp.resources` items (MCP `Resource` JSON); templates (no concrete `uri`) are skipped. */
export function parseResources(server: string, items: unknown[]): McpResource[] {
  const out: McpResource[] = [];
  for (const it of items) {
    const o = (it ?? {}) as Record<string, unknown>;
    const uri = str(o.uri);
    if (!uri) continue;
    out.push({ server, uri, name: str(o.title) ?? str(o.name) ?? uri, description: str(o.description), mime: str(o.mimeType) });
  }
  return out;
}

/** `mcp.prompts` items (MCP `Prompt` JSON). */
export function parsePrompts(server: string, items: unknown[]): McpPrompt[] {
  const out: McpPrompt[] = [];
  for (const it of items) {
    const o = (it ?? {}) as Record<string, unknown>;
    const name = str(o.name);
    if (!name) continue;
    const args = Array.isArray(o.arguments) ? o.arguments : [];
    out.push({
      server,
      name,
      description: str(o.description),
      arguments: args
        .map((a) => (a ?? {}) as Record<string, unknown>)
        .filter((a) => str(a.name))
        .map((a) => ({ name: a.name as string, description: str(a.description), required: a.required === true })),
    });
  }
  return out;
}

/** `/server:prompt key="value" …` (empty optional arguments left out; quotes escaped). */
export function promptCommand(p: McpPrompt, values: Record<string, string>): string {
  const args = p.arguments
    .filter((a) => (values[a.name] ?? "").trim() !== "")
    .map((a) => `${a.name}="${values[a.name].replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`);
  return [`/${p.server}:${p.name}`, ...args].join(" ");
}

export function missingArgs(p: McpPrompt, values: Record<string, string>): string[] {
  return p.arguments.filter((a) => a.required && !(values[a.name] ?? "").trim()).map((a) => a.name);
}

/** Connected servers' items via `fetch(name)`; servers that fail are listed as errors. */
function useServerItems<T>(fetch: (name: string) => Promise<T[]>) {
  const app = useApp();
  const [items, setItems] = useState<T[] | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    (async () => {
      const servers: McpServerInfo[] = await app.client.mcp
        .list({})
        .then((r) => r.servers)
        .catch(() => []);
      const ready = servers.filter((s) => s.status === "connected");
      const results = await Promise.allSettled(ready.map((s) => fetch(s.name)));
      if (!live) return;
      setItems(results.flatMap((r) => (r.status === "fulfilled" ? r.value : [])));
      setErrors(results.flatMap((r, i) => (r.status === "rejected" ? [`${ready[i].name}: ${app.describeError(r.reason)}`] : [])));
    })();
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app]);
  return { items, errors };
}

export function McpResourcePicker({ onPick, onClose }: { onPick: (r: McpResource) => Promise<void>; onClose: () => void }) {
  const app = useApp();
  const { items, errors } = useServerItems((name) => app.client.mcp.resources({ name }).then((r) => parseResources(name, r.items)));
  const [busy, setBusy] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const shown = (items ?? []).filter((r) => !filter || `${r.server} ${r.name} ${r.uri}`.toLowerCase().includes(filter.toLowerCase()));
  return (
    <Dialog open title={t("composer.mcpResource")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      {!items ? (
        t("common.loading")
      ) : (
        <div className="stack">
          <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={t("sidebar.search")} aria-label={t("sidebar.search")} />
          {shown.map((r) => (
            <button
              key={`${r.server} ${r.uri}`}
              className="list-button"
              disabled={busy != null}
              onClick={() => {
                setBusy(r.uri);
                void onPick(r).finally(() => setBusy(null));
              }}
            >
              <strong>{r.name}</strong> <span className="muted">
                {r.server} · {r.uri}
              </span>
              {busy === r.uri && <span className="muted"> · {t("common.loading")}</span>}
            </button>
          ))}
          {items.length === 0 && <p className="muted">{t("composer.mcpNoResources")}</p>}
          {errors.map((e) => (
            <p key={e} className="muted">
              {e}
            </p>
          ))}
        </div>
      )}
    </Dialog>
  );
}

export function McpPromptPicker({ onInsert, onClose }: { onInsert: (command: string) => void; onClose: () => void }) {
  const app = useApp();
  const { items, errors } = useServerItems((name) => app.client.mcp.prompts({ name }).then((r) => parsePrompts(name, r.items)));
  const [sel, setSel] = useState<McpPrompt | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  if (sel) {
    const missing = missingArgs(sel, values);
    return (
      <Dialog
        open
        title={`/${sel.server}:${sel.name}`}
        onClose={onClose}
        footer={
          <>
            <Button onClick={() => setSel(null)}>{t("settings.back")}</Button>
            <Button variant="primary" disabled={missing.length > 0} onClick={() => onInsert(promptCommand(sel, values))}>
              {t("composer.mcpInsert")}
            </Button>
          </>
        }
      >
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault();
            if (!missing.length) onInsert(promptCommand(sel, values));
          }}
        >
          {sel.description && <p className="muted">{sel.description}</p>}
          {sel.arguments.map((a) => (
            <label key={a.name} className="stack-tight">
              <span>
                {a.name}
                {a.required && " *"} {a.description && <span className="muted">· {a.description}</span>}
              </span>
              <Input value={values[a.name] ?? ""} required={a.required} onChange={(e) => setValues((v) => ({ ...v, [a.name]: e.target.value }))} />
            </label>
          ))}
          {!sel.arguments.length && <p className="muted">{t("composer.mcpNoArgs")}</p>}
        </form>
      </Dialog>
    );
  }
  return (
    <Dialog open title={t("composer.mcpPrompt")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      {!items ? (
        t("common.loading")
      ) : (
        <div className="stack">
          {items.map((p) => (
            <button
              key={`${p.server}:${p.name}`}
              className="list-button"
              onClick={() => {
                setValues({});
                setSel(p);
              }}
            >
              <strong>
                /{p.server}:{p.name}
              </strong>{" "}
              <span className="muted">{p.description}</span>
            </button>
          ))}
          {items.length === 0 && <p className="muted">{t("composer.mcpNoPrompts")}</p>}
          {errors.map((e) => (
            <p key={e} className="muted">
              {e}
            </p>
          ))}
        </div>
      )}
    </Dialog>
  );
}
