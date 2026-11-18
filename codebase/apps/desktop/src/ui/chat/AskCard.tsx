// Ask cards above the composer (05 §2.2, D25, D30, D21): permission, elicitation and
// question forms, sampling, browser confirmation and definition-hash approvals.
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Ask, AskAnswer, Mode, RuleScope } from "@genie/protocol";
import { Badge, Button, Icon, Input, Select, Switch, Textarea } from "@genie/ui";
import { formatCost, t } from "../../i18n";
import { readBlob } from "../../state/attachments";
import { useAppStatic } from "../context";
import { highlight } from "../../markdown/markdown";

export interface FormField {
  name: string;
  title: string;
  type: "string" | "number" | "integer" | "boolean" | "enum";
  options?: string[];
  required: boolean;
  description?: string;
  sensitive: boolean;
}

/** Flat form fields from an elicitation JSON Schema (MCP restricts it to primitives). */
export function formFields(schema: unknown, sensitive: string[] = []): FormField[] {
  const s = (schema ?? {}) as { properties?: Record<string, Record<string, unknown>>; required?: string[] };
  return Object.entries(s.properties ?? {}).map(([name, p]) => {
    const en = Array.isArray(p.enum) ? (p.enum as unknown[]).map(String) : undefined;
    const ty = en ? "enum" : (["number", "integer", "boolean"].includes(String(p.type)) ? p.type : "string") as FormField["type"];
    return {
      name,
      title: String(p.title ?? name),
      type: ty,
      options: en,
      required: (s.required ?? []).includes(name),
      description: p.description ? String(p.description) : undefined,
      sensitive: sensitive.includes(name) || p.format === "password",
    };
  });
}

export function AskCard({ ask, onAnswer, onBrowserTakeOver, permission = {} }: { ask: Ask; onAnswer: (a: AskAnswer) => Promise<void>; onBrowserTakeOver?: () => void; permission?: PermissionContext }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const app = useAppStatic();
  const answer = async (a: AskAnswer) => {
    setBusy(true);
    setError(null);
    try {
      await onAnswer(a);
    } catch (e) {
      setError(app.describeError(e));
      setBusy(false);
    }
  };
  return (
    <section className={`ask${ask.kind === "permission" ? " ask--permission" : ""}`} role="alertdialog" aria-live="assertive" aria-label={ask.kind}>
      {ask.subagent && (
        <Badge tone="info">
          <Icon name="sparkle" /> {t("ask.fromSubagent", { agent: ask.subagent.agent })}
        </Badge>
      )}
      {ask.kind === "permission" && <PermissionBody ask={ask} busy={busy} answer={answer} ctx={permission} />}
      {(ask.kind === "elicitation" || ask.kind === "question") && <FormBody ask={ask} busy={busy} answer={answer} />}
      {ask.kind === "sampling" && <SamplingBody ask={ask} busy={busy} answer={answer} />}
      {ask.kind === "browser_confirm" && <BrowserBody ask={ask} busy={busy} answer={answer} onTakeOver={onBrowserTakeOver} />}
      {ask.kind === "definition_hash" && <DefinitionBody ask={ask} busy={busy} answer={answer} />}
      {ask.kind === "unknown" && (
        <div className="row">
          <span className="grow">{ask.tool ?? "Genie is waiting for an answer this app can't show yet."}</span>
          <Button disabled={busy} onClick={() => void answer({ decision: "cancel" })}>
            {t("ask.decline")}
          </Button>
        </div>
      )}
      {error && <div className="error-text">{error}</div>}
    </section>
  );
}

type BodyProps = { ask: Ask; busy: boolean; answer: (a: AskAnswer) => Promise<void> };

function Preview({ ask }: { ask: Ask }) {
  const p = ask.preview;
  if (!p) return ask.resource ? <code className="ask__resource">{ask.resource}</code> : null;
  switch (p.type) {
    case "diff":
      return (
        <div>
          <div className="muted">{p.path}</div>
          <pre className="code diff" dangerouslySetInnerHTML={{ __html: highlight(p.unified, "diff") }} />
        </div>
      );
    case "command":
      return (
        <pre className="code">
          {p.cwd ? <span className="muted">{p.cwd} $ </span> : "$ "}
          {p.command}
        </pre>
      );
    case "url":
      return <code className="ask__resource">{p.url}</code>;
    case "mcp_args":
      return (
        <div>
          <div className="muted">
            {p.server} · {p.tool}
          </div>
          <pre className="code">{JSON.stringify(p.args, null, 2)}</pre>
        </div>
      );
    case "text":
      return <pre className="code">{p.text}</pre>;
    default:
      return ask.resource ? <code className="ask__resource">{ask.resource}</code> : null;
  }
}

const EDIT_TOOLS = new Set(["write", "edit", "multi_edit", "patch", "apply_patch"]);
const SHELL_TOOLS = new Set(["shell", "bash", "powershell"]);
const READ_TOOLS = new Set(["read", "grep", "glob", "list", "ls"]);
const baseName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
const dirName = (p: string) => p.replace(/[\\/][^\\/]*$/, "") || p;

/** Title, subtitle and icon of a permission ask, by tool (Claude-style "Run a command"). */
export function permissionHeading(ask: Ask): { icon: Parameters<typeof Icon>[0]["name"]; title: string; sub?: string } {
  const tool = ask.tool ?? "";
  const p = ask.preview;
  if (SHELL_TOOLS.has(tool)) return { icon: "terminal", title: t("perm.runCommand"), sub: p?.type === "command" && p.cwd ? t("perm.in", { place: baseName(p.cwd) }) : undefined };
  if (EDIT_TOOLS.has(tool) || p?.type === "diff") {
    const path = p?.type === "diff" ? p.path : (ask.resource ?? "");
    return { icon: "edit", title: t("perm.editFile", { file: baseName(path) || t("perm.aFile") }), sub: path ? dirName(path) : undefined };
  }
  if (READ_TOOLS.has(tool)) return { icon: "file", title: t("perm.readFile", { file: baseName(ask.resource ?? "") || t("perm.aFile") }), sub: ask.resource ? dirName(ask.resource) : undefined };
  if (tool === "web_fetch" || tool === "fetch") return { icon: "globe", title: t("perm.fetch"), sub: p?.type === "url" ? hostOf(p.url) : (ask.resource ?? undefined) };
  if (tool === "web_search") return { icon: "globe", title: t("perm.search") };
  if (tool.startsWith("mcp__") || p?.type === "mcp_args") {
    const server = p?.type === "mcp_args" ? p.server : tool.split("__")[1];
    const name = p?.type === "mcp_args" ? p.tool : (tool.split("__")[2] ?? tool);
    return { icon: "plug", title: t("perm.useTool", { tool: name }), sub: server ? t("perm.from", { server }) : undefined };
  }
  if (tool.startsWith("browser")) return { icon: "globe", title: t("perm.browser"), sub: ask.resource ?? undefined };
  if (tool === "skill" || tool === "skill_read") return { icon: "sparkle", title: t("perm.skill", { name: ask.resource ?? "" }) };
  return { icon: "lock", title: t("perm.useTool", { tool: tool || t("perm.aTool") }) };
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

const tick = (s: string) => `“${s}”`;
function joinList(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} ${t("perm.and")} ${items[items.length - 1]}`;
}

/** Plain-language text for what an "always" saves, e.g. "don't ask again for “ps” and
 *  “egrep” commands". `null` when the rules don't share one shape (the raw rules are
 *  shown instead). */
export function describeRules(rules: string[]): string | null {
  const parsed = rules.map((r) => {
    const m = /^([A-Za-z0-9_-]+)(?:\((.*)\))?$/.exec(r.trim());
    return m ? { tool: m[1], spec: m[2] } : { tool: r, spec: undefined };
  });
  const kinds = new Set(parsed.map((p) => (p.tool === "PowerShell" ? "Bash" : p.tool.startsWith("mcp__") ? "mcp" : p.tool)));
  if (kinds.size !== 1) return null;
  const kind = [...kinds][0];
  const specs = parsed.map((p) => p.spec ?? "");
  const path = (s: string) => (s.startsWith("//") ? `/${s.slice(2)}` : s.startsWith("/") ? s.slice(1) || "." : s).replace(/\/?\*\*$/, "/");
  switch (kind) {
    case "Bash": {
      const prefixes = specs.map((s) => (s.endsWith(":*") ? s.slice(0, -2) : null));
      if (prefixes.every((x) => x !== null)) return t("perm.dontAskCommands", { commands: joinList((prefixes as string[]).map(tick)) });
      if (specs.every((s) => s)) return t("perm.dontAskExact", { commands: joinList(specs.map(tick)) });
      return t("perm.allowShell");
    }
    case "Read":
      return t("perm.allowReading", { paths: joinList(specs.map((s) => tick(path(s) || "/"))) });
    case "Edit":
    case "Write":
      if (specs.length === 1 && (specs[0] === "/**" || specs[0] === "")) return t("perm.allowAllEdits");
      return t("perm.allowEdits", { paths: joinList(specs.map((s) => tick(path(s)))) });
    case "WebFetch":
      return t("perm.allowFetch", { domains: joinList(specs.map((s) => tick(s.replace(/^domain:/, "")))) });
    case "WebSearch":
      return t("perm.allowSearch");
    case "Browser":
      return t("perm.allowBrowser", { sites: joinList(specs.map((s) => tick(s.replace(/^site:/, "")))) });
    case "mcp":
      return t("perm.allowMcp", { tools: joinList(parsed.map((p) => tick(p.tool.split("__")[2] ?? p.tool.split("__")[1] ?? p.tool))) });
    default:
      return t("perm.allowTool", { tools: joinList(parsed.map((p) => tick(p.spec ? `${p.tool}(${p.spec})` : p.tool))) });
  }
}

function scopeText(scope: RuleScope, project?: string): string {
  switch (scope) {
    case "session":
      return t("perm.scope.session");
    case "local":
      return project ? t("perm.scope.project", { project }) : t("perm.scope.thisProject");
    case "user":
      return t("perm.scope.user");
    default:
      return "";
  }
}

interface PermOption {
  id: "once" | "always" | "accept_edits" | "deny";
  label: ReactNode;
  hint?: string;
  title?: string;
}

export interface PermissionContext {
  /** Code sessions: the project folder name for "in <project>". */
  project?: string;
  /** Current session mode; with `onSetMode`, edit asks offer "auto-accept edits". */
  mode?: Mode;
  onSetMode?: (mode: Mode) => Promise<void> | void;
  /** Take the keyboard (1/2/3, arrows, Enter, Esc) when the card appears. */
  autoFocus?: boolean;
}

function PermissionBody({ ask, busy, answer, ctx }: BodyProps & { ctx: PermissionContext }) {
  const suggestions = ask.save_suggestions ?? [];
  const rules = suggestions.map((s) => s.rule);
  const defaultScope: RuleScope = suggestions[0]?.scope ?? "session";
  const [scope, setScope] = useState<RuleScope>(defaultScope);
  const [active, setActive] = useState(0);
  const [denying, setDenying] = useState(false);
  const [reason, setReason] = useState("");
  const root = useRef<HTMLDivElement>(null);
  const reasonRef = useRef<HTMLTextAreaElement>(null);
  const head = permissionHeading(ask);
  const isEdit = EDIT_TOOLS.has(ask.tool ?? "") || ask.preview?.type === "diff";
  const hint = ask.annotations?.destructive_hint ? t("ask.destructive") : ask.annotations?.read_only_hint ? t("ask.readOnly") : null;

  const options: PermOption[] = [{ id: "once", label: t("perm.yes"), hint: "↵" }];
  if (isEdit && ctx.onSetMode && ctx.mode === "default") {
    options.push({ id: "accept_edits", label: t("perm.yesAcceptEdits") });
  } else if (rules.length > 0) {
    const what = describeRules(rules) ?? t("perm.allowTool", { tools: joinList(rules.map(tick)) });
    // The scope picker beside the label finishes the sentence ("… in AIO ▾").
    options.push({ id: "always", label: `${t("perm.yesAnd")} ${what}`, title: rules.join("\n") });
  }
  options.push({ id: "deny", label: t("perm.no"), hint: "esc" });

  useEffect(() => {
    if (!ctx.autoFocus) return;
    const el = document.activeElement as HTMLElement | null;
    // Don't steal the keyboard from someone typing a steer message.
    const typing = el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT" || el.isContentEditable) && ((el as HTMLTextAreaElement).value ?? el.textContent ?? "").length > 0;
    if (!typing) root.current?.focus({ preventScroll: true });
  }, [ctx.autoFocus]);
  useEffect(() => {
    if (denying) reasonRef.current?.focus();
  }, [denying]);

  const choose = async (id: PermOption["id"]) => {
    if (busy) return;
    switch (id) {
      case "once":
        return answer({ decision: "once" });
      case "always":
        return answer({ decision: "always", scope });
      case "accept_edits":
        await ctx.onSetMode?.("accept_edits");
        return answer({ decision: "once" });
      case "deny":
        if (!denying) {
          setDenying(true);
          setActive(options.length - 1);
          return;
        }
        return answer({ decision: "reject", message: reason.trim() || null });
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (busy || e.altKey || e.ctrlKey || e.metaKey) return;
    const inReason = e.target === reasonRef.current;
    if (inReason) {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void answer({ decision: "reject", message: reason.trim() || null });
      } else if (e.key === "Escape") {
        e.preventDefault();
        if (reason) setReason("");
        else void answer({ decision: "reject" });
      }
      return;
    }
    const n = Number(e.key);
    if (Number.isInteger(n) && n >= 1 && n <= options.length) {
      e.preventDefault();
      setActive(n - 1);
      void choose(options[n - 1].id);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => (a + (e.key === "ArrowDown" ? 1 : options.length - 1)) % options.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      void choose(options[active].id);
    } else if (e.key === "Escape") {
      e.preventDefault();
      void answer({ decision: "reject" });
    }
  };

  const scopes: RuleScope[] = ctx.project ? ["local", "session", "user"] : ["user", "session"];
  return (
    <div className="perm" ref={root} tabIndex={-1} onKeyDown={onKeyDown} aria-busy={busy}>
      <div className="perm__head">
        <span className="perm__icon">
          <Icon name={head.icon} size={15} />
        </span>
        <span className="perm__title">{head.title}</span>
        {head.sub && <span className="perm__sub" title={head.sub}>{head.sub}</span>}
      </div>
      <PermPreview ask={ask} />
      {hint && <div className="perm__note">{t("ask.annotations", { hint })}</div>}
      <div className="perm__question">{t("perm.question")}</div>
      <div className="perm__options" role="listbox" aria-label={t("perm.question")}>
        {options.map((o, i) => (
          <div key={o.id} className={`perm__opt${i === active ? " is-active" : ""}${o.id === "deny" ? " perm__opt--deny" : ""}`}>
            <button
              type="button"
              role="option"
              aria-selected={i === active}
              className="perm__optbtn"
              disabled={busy}
              title={o.title}
              onMouseEnter={() => setActive(i)}
              onClick={() => void choose(o.id)}
            >
              <span className="perm__num">{i + 1}</span>
              <span className="perm__label">{o.label}</span>
              {o.hint && <span className="perm__key">{o.hint}</span>}
            </button>
            {o.id === "always" && suggestions.length > 0 && (
              <select className="perm__scope" aria-label={t("perm.scopeLabel")} value={scope} disabled={busy} onChange={(e) => setScope(e.target.value as RuleScope)} onClick={(e) => e.stopPropagation()}>
                {scopes.map((sc) => (
                  <option key={sc} value={sc}>
                    {scopeText(sc, ctx.project)}
                  </option>
                ))}
              </select>
            )}
          </div>
        ))}
      </div>
      {denying && (
        <div className="perm__reason">
          <textarea ref={reasonRef} rows={1} value={reason} disabled={busy} onChange={(e) => setReason(e.target.value)} placeholder={t("perm.reasonPlaceholder")} aria-label={t("perm.reasonPlaceholder")} />
          <button type="button" className="perm__send" disabled={busy} onClick={() => void answer({ decision: "reject", message: reason.trim() || null })}>
            {reason.trim() ? t("perm.sendFeedback") : t("ask.deny")}
          </button>
        </div>
      )}
      <div className="perm__foot">{denying ? t("perm.footDeny") : t("perm.foot")}</div>
    </div>
  );
}

function PermPreview({ ask }: { ask: Ask }) {
  const p = ask.preview;
  if (p?.type === "command") return <pre className="perm__code perm__code--cmd">{p.command}</pre>;
  if (p?.type === "diff") return <pre className="perm__code perm__code--diff" dangerouslySetInnerHTML={{ __html: highlight(p.unified, "diff") }} />;
  if (p?.type === "url") return <div className="perm__code perm__code--line">{p.url}</div>;
  if (p?.type === "mcp_args") return <pre className="perm__code">{JSON.stringify(p.args, null, 2)}</pre>;
  if (p?.type === "text") return <div className="perm__note perm__note--warn">{p.text}</div>;
  return ask.resource ? <div className="perm__code perm__code--line">{ask.resource}</div> : null;
}

function FormBody({ ask, busy, answer }: BodyProps) {
  const fields = formFields(ask.form, ask.sensitive_fields);
  const [values, setValues] = useState<Record<string, unknown>>({});
  const set = (k: string, v: unknown) => setValues((x) => ({ ...x, [k]: v }));
  const missing = fields.some((f) => f.required && (values[f.name] === undefined || values[f.name] === ""));
  return (
    <form
      className="stack"
      onSubmit={(e) => {
        e.preventDefault();
        void answer({ decision: "submit", data: values });
      }}
    >
      <div className="ask__title">
        <Icon name="info" /> {ask.kind === "question" ? t("ask.question") : (ask.tool ?? t("ask.question"))}
      </div>
      {ask.preview?.type === "text" && <div>{ask.preview.text}</div>}
      {fields.map((f) => (
        <label key={f.name} className="stack-tight">
          <span>
            {f.title}
            {f.required && " *"}
          </span>
          {f.description && <span className="muted">{f.description}</span>}
          {f.type === "boolean" ? (
            <Switch checked={!!values[f.name]} onChange={(v) => set(f.name, v)} aria-label={f.title} />
          ) : f.type === "enum" ? (
            <Select value={String(values[f.name] ?? "")} onChange={(v) => set(f.name, v)} options={[{ value: "", label: "—" }, ...(f.options ?? []).map((o) => ({ value: o, label: o }))]} />
          ) : (
            <Input
              type={f.sensitive ? "password" : f.type === "string" ? "text" : "number"}
              autoComplete="off"
              value={String(values[f.name] ?? "")}
              onChange={(e) => set(f.name, f.type === "string" ? e.target.value : Number(e.target.value))}
            />
          )}
        </label>
      ))}
      <div className="row">
        <Button type="submit" variant="primary" disabled={busy || missing}>
          {t("ask.submit")}
        </Button>
        <Button disabled={busy} onClick={() => void answer({ decision: "cancel" })}>
          {t("ask.decline")}
        </Button>
      </div>
    </form>
  );
}

function SamplingBody({ ask, busy, answer }: BodyProps) {
  const s = ask.sampling;
  return (
    <>
      <div className="ask__title">
        <Icon name="sparkle" /> {t("ask.sampling", { server: s?.server ?? "A server" })}
      </div>
      {s && (
        <>
          <pre className="code">{s.messages_preview}</pre>
          <div className="muted">{t("ask.samplingCost", { model: s.model, cost: formatCost(s.est_cost_usd), budget: s.budget_left_tokens })}</div>
        </>
      )}
      <div className="row">
        <Button variant="primary" disabled={busy} onClick={() => void answer({ decision: "once" })}>
          {t("ask.approve")}
        </Button>
        <Button disabled={busy} onClick={() => void answer({ decision: "reject" })}>
          {t("ask.deny")}
        </Button>
      </div>
    </>
  );
}

function BrowserBody({ ask, busy, answer, onTakeOver }: BodyProps & { onTakeOver?: () => void }) {
  const b = ask.browser;
  const app = useAppStatic();
  const [img, setImg] = useState<string | null>(null);
  useEffect(() => {
    if (!b?.screenshot_blob) return;
    let url: string | null = null;
    readBlob(app.client, b.screenshot_blob, 8 * 1024 * 1024)
      .then((bytes) => {
        url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: "image/png" }));
        setImg(url);
      })
      .catch(() => {});
    return () => {
      if (url) URL.revokeObjectURL(url);
    };
  }, [app, b?.screenshot_blob]);
  return (
    <>
      <div className="ask__title">
        <Icon name="globe" /> {t("ask.browser", { action: b?.action ?? "act", origin: b?.origin ?? "" })}
      </div>
      {img && <img className="ask__shot" src={img} alt={b?.summary ?? ""} />}
      {b?.summary && <div>{b.summary}</div>}
      <div className="row">
        <Button variant="primary" disabled={busy} onClick={() => void answer({ decision: "once" })}>
          {t("ask.approve")}
        </Button>
        {onTakeOver && (
          <Button disabled={busy} onClick={onTakeOver}>
            {t("ask.takeOver")}
          </Button>
        )}
        <Button disabled={busy} onClick={() => void answer({ decision: "reject" })}>
          {t("ask.deny")}
        </Button>
      </div>
    </>
  );
}

function DefinitionBody({ ask, busy, answer }: BodyProps) {
  const [always, setAlways] = useState(false);
  return (
    <>
      <div className="ask__title">
        <Icon name="warning" /> {t("ask.definition", { kind: ask.tool ?? "definition", name: ask.resource ?? "" })}
      </div>
      <Preview ask={ask} />
      <Switch checked={always} onChange={setAlways} label="Remember for this project" />
      <div className="row">
        <Button variant="primary" disabled={busy} onClick={() => void answer({ decision: always ? "always" : "once" })}>
          {t("ask.approve")}
        </Button>
        <Button disabled={busy} onClick={() => void answer({ decision: "reject" })}>
          {t("ask.skip")}
        </Button>
      </div>
      <Textarea hidden readOnly value="" aria-hidden />
    </>
  );
}
