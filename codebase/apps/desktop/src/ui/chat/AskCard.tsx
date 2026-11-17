// Ask cards above the composer (05 §2.2, D25, D30, D21): permission, elicitation and
// question forms, sampling, browser confirmation and definition-hash approvals.
import { useEffect, useState } from "react";
import type { Ask, AskAnswer, RuleScope } from "@genie/protocol";
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

export function AskCard({ ask, onAnswer, onBrowserTakeOver }: { ask: Ask; onAnswer: (a: AskAnswer) => Promise<void>; onBrowserTakeOver?: () => void }) {
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
    <section className="ask" role="alertdialog" aria-live="assertive" aria-label={ask.kind}>
      {ask.subagent && (
        <Badge tone="info">
          <Icon name="sparkle" /> {t("ask.fromSubagent", { agent: ask.subagent.agent })}
        </Badge>
      )}
      {ask.kind === "permission" && <PermissionBody ask={ask} busy={busy} answer={answer} />}
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

function PermissionBody({ ask, busy, answer }: BodyProps) {
  const [reason, setReason] = useState("");
  const [denying, setDenying] = useState(false);
  const suggestions = ask.save_suggestions ?? [];
  const [scope, setScope] = useState<RuleScope>(suggestions[0]?.scope ?? "session");
  const hint = ask.annotations?.destructive_hint ? t("ask.destructive") : ask.annotations?.read_only_hint ? t("ask.readOnly") : null;
  return (
    <>
      <div className="ask__title">
        <Icon name="lock" /> {t("ask.permission", { tool: ask.tool ?? "this" })}
      </div>
      <Preview ask={ask} />
      {hint && <div className="muted">{t("ask.annotations", { hint })}</div>}
      {denying && <Input value={reason} onChange={(e) => setReason(e.target.value)} placeholder={t("ask.reason")} aria-label={t("ask.reason")} autoFocus />}
      <div className="row wrap">
        <Button variant="primary" disabled={busy} onClick={() => void answer({ decision: "once" })}>
          {t("ask.allowOnce")}
        </Button>
        <Button disabled={busy} onClick={() => void answer({ decision: "always", scope })} title={suggestions.find((s) => s.scope === scope)?.rule}>
          {t("ask.always")}
        </Button>
        {suggestions.length > 1 && (
          <Select
            aria-label="Scope"
            value={scope}
            onChange={(v) => setScope(v as RuleScope)}
            options={suggestions.map((s) => ({ value: s.scope, label: t("ask.saveAs", { rule: s.rule, scope: s.scope }) }))}
            style={{ width: "auto" }}
          />
        )}
        <Button
          variant="ghost"
          disabled={busy}
          onClick={() => {
            if (!denying) setDenying(true);
            else void answer({ decision: "reject", message: reason || null });
          }}
        >
          {t("ask.deny")}
        </Button>
      </div>
    </>
  );
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
