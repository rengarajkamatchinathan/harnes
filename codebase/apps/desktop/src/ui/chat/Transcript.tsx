// The virtualised transcript (05 §2.2, §7): only visible messages are mounted; rows are
// memoised on `rev`; the view sticks to the bottom while streaming unless the user scrolls.
import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Citation, Part } from "@genie/protocol";
import { Badge, Button, Icon, IconButton, Textarea } from "@genie/ui";
import { t } from "../../i18n";
import { readBlob } from "../../state/attachments";
import type { MessageView, PartView, SessionModel, ToolView } from "../../state/sessionModel";
import { siblingInfo } from "../../state/sessionModel";
import type { SessionStore } from "../../state/session";
import { useAppStatic } from "../context";
import { MarkdownText, StreamingText } from "./Markdown";
import { headTail, resultText, summarizeTool } from "./toolInfo";

export function Transcript({ store, compact, onRewind }: { store: SessionStore; compact?: boolean; onRewind?: (m: MessageView) => void }) {
  const model = store.model;
  const parentRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const items = model.messages;
  const virt = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 160,
    overscan: 4,
    getItemKey: (i) => items[i].id,
  });

  // After every commit: keep the bottom in view while streaming (unless the user scrolled up).
  useLayoutEffect(() => {
    const el = parentRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  useEffect(() => {
    if (items.length) virt.scrollToIndex(items.length - 1, { align: "end" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store.loading]);

  return (
    <div
      ref={parentRef}
      className="transcript"
      role="log"
      aria-live="polite"
      aria-busy={store.busy}
      tabIndex={0}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
      }}
    >
      <div style={{ height: virt.getTotalSize(), position: "relative" }}>
        {virt.getVirtualItems().map((vi) => {
          const m = items[vi.index];
          return (
            <div key={vi.key} data-index={vi.index} ref={virt.measureElement} className="transcript__item" style={{ transform: `translateY(${vi.start}px)` }}>
              <MessageRow msg={m} rev={m.rev} store={store} model={model} compact={!!compact} onRewind={onRewind} />
            </div>
          );
        })}
      </div>
    </div>
  );
}

const MessageRow = memo(
  function MessageRow({ msg, store, model, compact, onRewind }: { msg: MessageView; rev: number; store: SessionStore; model: SessionModel; compact: boolean; onRewind?: (m: MessageView) => void }) {
    const [editing, setEditing] = useState(false);
    const [draft, setDraft] = useState("");
    const app = useAppStatic();
    const text = msg.parts
      .filter((p) => p.kind === "text")
      .map((p) => p.text)
      .join("\n\n");
    const sib = siblingInfo(model, msg.id);
    const copy = () => void navigator.clipboard?.writeText(text);
    const isUser = msg.role === "user";
    const streaming = msg.parts.some((p) => p.streaming);

    if (isUser && editing)
      return (
        <div className="msg msg--user">
          <form
            className="stack edit-box"
            onSubmit={(e) => {
              e.preventDefault();
              setEditing(false);
              void store.edit(msg, draft).catch((err) => app.toast({ title: app.describeError(err), tone: "danger" }));
            }}
          >
            <Textarea value={draft} onChange={(e) => setDraft(e.target.value)} autoFocus aria-label={t("chat.edit")} />
            <div className="row end">
              <Button onClick={() => setEditing(false)}>{t("chat.cancel")}</Button>
              <Button type="submit" variant="primary" disabled={!draft.trim()}>
                {t("chat.save")}
              </Button>
            </div>
          </form>
        </div>
      );

    return (
      <article className={`msg ${isUser ? "msg--user" : "msg--assistant"}`} aria-label={isUser ? "You" : "Genie"}>
        {isUser ? (
          <div className="bubble">
            {text && <div className="bubble__text">{text}</div>}
            {msg.input?.some((p) => p.type !== "text") && (
              <div className="row wrap">
                {msg.input
                  .filter((p) => p.type !== "text")
                  .map((p, i) => (
                    <Badge key={i}>
                      <Icon name={p.type === "attachment" ? "paperclip" : p.type === "skill" ? "sparkle" : "file"} />
                      {p.type === "attachment" ? p.name : p.type === "skill" ? `/${p.id}` : p.type === "file_ref" ? p.path : p.type === "resource" ? p.uri : ""}
                    </Badge>
                  ))}
              </div>
            )}
          </div>
        ) : (
          <div className="assistant">
            {msg.parts.map((p) => (
              <PartRow key={p.id} part={p} model={model} compact={compact} />
            ))}
            {!msg.parts.length && <span className="caret" aria-hidden />}
          </div>
        )}
        {!streaming && (
          <div className="msg__actions">
            {sib && (
              <span className="branch" aria-label={`${sib.index + 1}/${sib.ids.length}`}>
                <IconButton icon="chevronLeft" size="sm" label={t("chat.prevBranch")} disabled={sib.index <= 0} onClick={() => void store.switchBranch(sib.ids[sib.index - 1])} />
                {sib.index + 1}/{sib.ids.length}
                <IconButton icon="chevronRight" size="sm" label={t("chat.nextBranch")} disabled={sib.index >= sib.ids.length - 1} onClick={() => void store.switchBranch(sib.ids[sib.index + 1])} />
              </span>
            )}
            {text && <IconButton icon="copy" size="sm" label={t("chat.copy")} onClick={copy} />}
            {isUser ? (
              <IconButton
                icon="edit"
                size="sm"
                label={t("chat.edit")}
                onClick={() => {
                  setDraft(text);
                  setEditing(true);
                }}
              />
            ) : (
              <IconButton icon="refresh" size="sm" label={t("chat.regenerate")} onClick={() => void store.regenerate(msg.id).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }))} />
            )}
            {onRewind && isUser && <IconButton icon="refresh" size="sm" label={t("code.rewind")} onClick={() => onRewind(msg)} />}
          </div>
        )}
      </article>
    );
  },
  (a, b) => a.msg === b.msg && a.rev === b.rev && a.compact === b.compact && a.model.tools === b.model.tools,
);

function PartRow({ part, model, compact }: { part: PartView; model: SessionModel; compact: boolean }) {
  switch (part.kind) {
    case "text": {
      const cites = part.final?.type === "text" ? part.final.citations : undefined;
      return part.streaming ? (
        <StreamingText part={part} />
      ) : (
        <>
          <MarkdownText text={part.text} citations={cites} />
          {cites && cites.length > 0 && <Sources cites={cites} />}
        </>
      );
    }
    case "reasoning":
      return <Thought part={part} />;
    case "tool_call":
      return <ToolRow part={part} model={model} compact={compact} />;
    case "tool_result":
      return null; // shown inside its tool row
    case "media":
      return part.final?.type === "media" ? <Media part={part.final} /> : null;
    case "compaction": {
      const f = part.final?.type === "compaction" ? part.final : null;
      return <Compaction summary={f?.summary ?? null} native={!!f?.native} />;
    }
    default:
      return null;
  }
}

function Thought({ part }: { part: PartView }) {
  const redacted = part.final?.type === "reasoning" && part.final.redacted;
  return (
    <details className="thought">
      <summary>
        <Icon name="thought" /> {part.streaming ? t("chat.thinking") : redacted ? t("chat.thoughtOmitted") : t("chat.thoughtFor")}
      </summary>
      {!redacted && <div className="thought__body">{part.text}</div>}
    </details>
  );
}

function Compaction({ summary, native }: { summary: string | null; native: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="divider" role="separator">
      <span>
        {t("chat.compacted")}
        {native ? " · Summarised by the provider" : ""}
        {summary && (
          <>
            {" · "}
            <button className="linklike" onClick={() => setOpen((o) => !o)}>
              {t("chat.viewSummary")}
            </button>
          </>
        )}
      </span>
      {open && summary && <div className="divider__summary">{summary}</div>}
    </div>
  );
}

function Sources({ cites }: { cites: Citation[] }) {
  return (
    <div className="sources" aria-label={t("chat.sources")}>
      {cites.map((c, i) => {
        const domain = c.url ? safeDomain(c.url) : (c.title ?? "");
        return (
          <a key={i} href="#" data-href={c.url ?? undefined} className="source-chip" title={c.title ?? c.url ?? ""}>
            <span className="source-chip__n">{i + 1}</span>
            {domain}
          </a>
        );
      })}
    </div>
  );
}

const safeDomain = (u: string) => {
  try {
    return new URL(u).hostname.replace(/^www\./, "");
  } catch {
    return u;
  }
};

function findResult(model: SessionModel, tool: ToolView | undefined): Part | null {
  if (!tool?.resultPartId) return null;
  for (let i = model.messages.length - 1; i >= 0; i--) for (const p of model.messages[i].parts) if (p.id === tool.resultPartId) return p.final;
  return null;
}

function ToolRow({ part, model, compact }: { part: PartView; model: SessionModel; compact: boolean }) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState(false);
  const app = useAppStatic();
  const call = part.final?.type === "tool_call" ? part.final : null;
  const tool = call ? model.tools.get(call.call_id) : undefined;
  const name = call?.name ?? tool?.name ?? "tool";
  let input: unknown = call?.input;
  if (!call && part.json) {
    try {
      input = JSON.parse(part.json);
    } catch {
      input = undefined;
    }
  }
  const result = findResult(model, tool);
  const content = result?.type === "tool_result" ? result.content : undefined;
  const sum = summarizeTool(name, input, tool?.outcome ?? null, content);
  const running = !tool?.outcome && (part.streaming || !!tool);
  const status = tool?.outcome === "blocked" ? t("tool.blocked") : tool?.outcome === "declined" ? t("tool.declined") : tool?.outcome === "error" ? t("tool.failed") : null;
  const out = headTail(resultText(content));

  if (sum.memory && !compact)
    return (
      <div className="memory-chip">
        <Badge tone="accent">
          <Icon name="memory" /> {t("tool.memory")}
        </Badge>
        <button className="linklike" onClick={() => app.openDeepLink("genie://settings/genie/memory")}>
          View
        </button>
      </div>
    );
  if (sum.artifact && !compact)
    return (
      <button className="artifact-card" onClick={() => app.setUi({ panelOpen: true, panelTab: "artifact" })}>
        <Icon name="file" />
        <span>
          <strong>{sum.title}</strong>
          <span className="muted"> · {running ? t("common.loading") : t("common.open")}</span>
        </span>
      </button>
    );

  return (
    <div className={`tool ${compact ? "tool--compact" : ""}`}>
      <button className="tool__head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <Icon name={open ? "chevronDown" : "chevronRight"} />
        <Icon name={sum.icon} />
        <span className="tool__title">{sum.title}</span>
        {sum.server && <Badge>{sum.server}</Badge>}
        {running && <span className="g-spinner" aria-label={t("tool.running", { name })} />}
        {status && <Badge tone={tool?.outcome === "error" ? "danger" : "warning"}>{status}</Badge>}
        {tool?.progress?.message && <span className="muted">{tool.progress.message}</span>}
      </button>
      {tool?.error && <div className="tool__error">{tool.error.message}</div>}
      {open && (
        <div className="tool__body">
          <div className="tool__label">{t("tool.arguments")}</div>
          <pre className="code">{input === undefined ? part.json : JSON.stringify(input, null, 2)}</pre>
          {(content !== undefined || tool?.progress?.output_tail) && (
            <>
              <div className="tool__label">{t("tool.result")}</div>
              <pre className="code">{content !== undefined ? (full ? resultText(content) : out.text) : tool?.progress?.output_tail}</pre>
              {out.hidden > 0 && !full && (
                <button className="linklike" onClick={() => setFull(true)}>
                  Show full ({out.hidden} more lines)
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function Media({ part }: { part: Extract<Part, { type: "media" }> }) {
  const app = useAppStatic();
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!part.mime.startsWith("image/")) return;
    let u: string | null = null;
    readBlob(app.client, part.blob_id, 16 * 1024 * 1024)
      .then((bytes) => {
        u = URL.createObjectURL(new Blob([bytes as BlobPart], { type: part.mime }));
        setUrl(u);
      })
      .catch(() => {});
    return () => {
      if (u) URL.revokeObjectURL(u);
    };
  }, [app, part.blob_id, part.mime]);
  if (url) return <img className="media" src={url} alt={part.name ?? ""} />;
  return (
    <Badge>
      <Icon name="file" /> {part.name ?? part.mime}
    </Badge>
  );
}
