// The virtualised transcript (05 §2.2, §7): only visible messages are mounted; rows are
// memoised on `rev`; the view sticks to the bottom while streaming unless the user scrolls.
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Citation, Part } from "@genie/protocol";
import { Badge, Button, Icon, IconButton, Textarea } from "@genie/ui";
import { t } from "../../i18n";
import type { TurnUsage } from "../../state/sessionModel";
import { readBlob } from "../../state/attachments";
import type { MessageView, PartView, SessionModel, ToolView } from "../../state/sessionModel";
import { siblingInfo } from "../../state/sessionModel";
import type { SessionStore } from "../../state/session";
import { useEmitter } from "../../state/emitter";
import { useAppStatic } from "../context";
import { GenieMark } from "../GenieMark";
import { TextPart } from "./Markdown";
import { CompareAnswersButton } from "./Compare";
import { QuestionStepper } from "./QuestionStepper";
import { ReadyToCommit } from "../code/ReadyToCommit";
import { parseQuestions } from "./questions";
import { UsageChip } from "./Usage";
import { turnStats } from "./usageStats";
import { registerTranscriptScroll } from "./scrollBus";

export { UsageChip };
import { type GeneratedImage, headTail, resultText, summarizeTool, toolActivity, toolGroupSummary } from "./toolInfo";

/** The question stepper under the latest reply when that reply asks the user several questions. */
function LastReplyQuestions({ msg, store }: { msg: MessageView; store: SessionStore }) {
  const text = msg.parts
    .filter((p) => p.kind === "text")
    .map((p) => p.text)
    .join("\n\n");
  const questions = useMemo(() => parseQuestions(text), [text]);
  return questions ? <QuestionStepper key={msg.id} messageId={msg.id} questions={questions} store={store} /> : null;
}

/** How far above the end (px) the view must be before "Scroll to bottom" shows. */
const JUMP_GAP = 120;

export function Transcript({ store, compact, onRewind, lead }: { store: SessionStore; compact?: boolean; onRewind?: (m: MessageView) => void; /** A line above the first message (scrolls with it). */ lead?: ReactNode }) {
  const model = store.model;
  const parentRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // The jump button follows where the view really is (far enough above the end), not the
  // follow flag: a stray upward wheel tick at the bottom, or on a transcript too short to
  // scroll, used to leave it showing although nothing was hidden below.
  const [away, setAway] = useState(false);
  const syncAway = (el: HTMLElement) => setAway(el.scrollHeight - el.scrollTop - el.clientHeight > JUMP_GAP);
  // True while a "scroll to bottom" animation runs, so its own scroll events don't unstick.
  const jumping = useRef(false);
  const busy = store.busy;
  const code = model.info?.kind === "code";
  const items = useMemo(() => buildRows(model.messages), [model.messages, model.rev]); // eslint-disable-line react-hooks/exhaustive-deps
  const virt = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 120,
    overscan: 4,
    getItemKey: (i) => items[i].key,
  });

  // Keep the bottom in view while content grows (streaming, new rows) unless the user scrolled
  // up. Only on growth: renders caused by the user's own scrolling must never pull them back.
  const lastHeight = useRef(0);
  const lastTop = useRef(0);
  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const h = el.scrollHeight;
    if (stick.current && h !== lastHeight.current) el.scrollTop = h;
    // Nothing to scroll: there is no "above", so keep following.
    if (h <= el.clientHeight + 1) stick.current = true;
    lastHeight.current = h;
    syncAway(el);
  });
  // Any upward intent (wheel, touchpad, keys) stops following at once, however small the step,
  // as long as there is somewhere to scroll to.
  const unstick = () => {
    jumping.current = false;
    const el = parentRef.current;
    if (!stick.current || !el || el.scrollHeight <= el.clientHeight + 1) return;
    stick.current = false;
  };
  useEffect(() => {
    if (items.length) virt.scrollToIndex(items.length - 1, { align: "end" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store.loading]);

  // Rows that appear after the transcript has loaded ease in once; rows remounted by the
  // virtualiser when scrolling back are not "new". Keys first seen > 600 ms ago never animate.
  const born = useRef<Map<string, number> | null>(null);
  if (!store.loading && born.current === null) born.current = new Map(items.map((r) => [r.key, 0]));
  const now = performance.now();
  if (born.current) for (const r of items) if (!born.current.has(r.key)) born.current.set(r.key, now);
  const isNew = (key: string) => {
    const t0 = born.current?.get(key);
    return !!t0 && now - t0 < 600;
  };
  let lastAssistant = -1;
  for (let i = items.length - 1; i >= 0; i--) {
    const r = items[i];
    if (r.kind === "msg" && r.msg.role === "assistant") {
      lastAssistant = i;
      break;
    }
  }

  // Scroll to a message on request (context meter): stop following the bottom, then flash the row.
  const [flash, setFlash] = useState<string | null>(null);
  const latest = useRef({ items, messages: model.messages });
  latest.current = { items, messages: model.messages };
  useEffect(
    () =>
      registerTranscriptScroll(store.id, (messageId) => {
        const { items: rows, messages } = latest.current;
        const i = rowIndexOf(messages, rows, messageId);
        if (i < 0) return;
        const key = rows[i].key;
        stick.current = false;
        jumping.current = false;
        virt.scrollToIndex(i, { align: "start" });
        setFlash(key);
        window.setTimeout(() => setFlash((f) => (f === key ? null : f)), 1600);
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [store.id],
  );

  const jumpToBottom = () => {
    const el = parentRef.current;
    if (!el) return;
    stick.current = true;
    jumping.current = true;
    setAway(false);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="transcript-wrap">
    <div
      ref={parentRef}
      className="transcript"
      role="log"
      aria-live="polite"
      aria-busy={store.busy}
      tabIndex={0}
      onWheel={(e) => e.deltaY < 0 && unstick()}
      onKeyDown={(e) => ["ArrowUp", "PageUp", "Home"].includes(e.key) && unstick()}
      onScroll={(e) => {
        const el = e.currentTarget;
        const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
        const down = el.scrollTop > lastTop.current;
        lastTop.current = el.scrollTop;
        // Moving away from the bottom (scrollbar drag, touch) unsticks; scrolling down to the
        // very bottom re-sticks. Never re-stick on an upward move, however close to the end.
        // The jump animation's own scroll events are neither.
        if (jumping.current) {
          if (gap < 4) jumping.current = false;
        } else if (stick.current && gap > 48) unstick();
        else if (!stick.current && down && gap < 4) stick.current = true;
        syncAway(el);
      }}
    >
      <div style={{ height: virt.getTotalSize(), position: "relative" }}>
        {virt.getVirtualItems().map((vi) => {
          const row = items[vi.index];
          return (
            <div
              key={vi.key}
              data-index={vi.index}
              ref={virt.measureElement}
              className={`transcript__item${isNew(row.key) ? " is-new" : ""}${vi.index === lastAssistant && !busy ? " is-last" : ""}${flash === row.key ? " is-flash" : ""}`}
              style={{ transform: `translateY(${vi.start}px)` }}
            >
              {vi.index === 0 && lead}
              {row.kind === "msg" ? (
                <>
                  <MessageRow msg={row.msg} rev={row.msg.rev} store={store} model={model} tools={model.tools} busy={busy} compact={!!compact} onRewind={onRewind} turnUsage={row.msg.turnId ? model.turnUsage.get(row.msg.turnId) : undefined} />
                  {vi.index === lastAssistant && vi.index === items.length - 1 && !busy && <LastReplyQuestions msg={row.msg} store={store} />}
                </>
              ) : (
                <div className="msg msg--assistant msg--tools">
                  <ToolGroup parts={row.parts} model={model} busy={busy} compact={!!compact} />
                </div>
              )}
              {code && !busy && !store.loading && vi.index === items.length - 1 && <ReadyToCommit store={store} />}
            </div>
          );
        })}
      </div>
      <TurnIndicator store={store} />
    </div>
    {away && (
      <IconButton icon="arrowDown" label={t("chat.scrollToBottom")} variant="secondary" className="transcript__jump" onClick={jumpToBottom} />
    )}
    </div>
  );
}

const MessageRow = memo(
  function MessageRow({ msg, store, model, busy, compact, onRewind }: { msg: MessageView; rev: number; store: SessionStore; model: SessionModel; tools: SessionModel["tools"]; busy: boolean; compact: boolean; onRewind?: (m: MessageView) => void; turnUsage?: TurnUsage }) {
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

    if (isUser && msg.synthetic) return <Notice text={text} />;

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
            {msg.input
              ?.filter((p) => p.type === "command")
              .map((p, i) => (
                <div key={`cmd${i}`} className="bubble__text">
                  <span className="slash-token">/{p.name}</span>
                  {p.args ? ` ${p.args}` : ""}
                </div>
              ))}
            {text && <div className="bubble__text">{text}</div>}
            {msg.input?.some((p) => p.type !== "text" && p.type !== "command") && (
              <div className="row wrap">
                {msg.input
                  .filter((p) => p.type !== "text" && p.type !== "command")
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
            {chunkParts(msg.parts, model).map((c) =>
              c.kind === "tools" ? <ToolGroup key={c.key} parts={c.parts} model={model} busy={busy} compact={compact} /> : <PartRow key={c.part.id} part={c.part} model={model} busy={busy} compact={compact} />,
            )}
          </div>
        )}
        {/* While streaming the row keeps its height (empty) so it doesn't push content when it appears. */}
        {!isUser && !text.trim() ? null : streaming ? (
          <div className="msg__actions" aria-hidden />
        ) : (
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
            {!isUser && turnStats(msg, model) && <CompareAnswersButton store={store} msg={msg} />}
            {!isUser && turnStats(msg, model) && <UsageChip u={turnStats(msg, model)!} />}
            {onRewind && isUser && <IconButton icon="refresh" size="sm" label={t("code.rewind")} onClick={() => onRewind(msg)} />}
          </div>
        )}
      </article>
    );
  },
  // `tools` and `busy` are passed by value: comparing `a.model.tools` with `b.model.tools` read the
  // same mutable model twice, so rows never re-rendered when a tool finished (stuck spinners).
  (a, b) => a.msg === b.msg && a.rev === b.rev && a.compact === b.compact && a.tools === b.tools && a.busy === b.busy && a.turnUsage === b.turnUsage,
);

function PartRow({ part, model, busy, compact }: { part: PartView; model: SessionModel; busy: boolean; compact: boolean }) {
  switch (part.kind) {
    case "text": {
      const cites = part.final?.type === "text" ? part.final.citations : undefined;
      return (
        <>
          <TextPart part={part} citations={cites} />
          {!part.streaming && cites && cites.length > 0 && <Sources cites={cites} />}
        </>
      );
    }
    case "reasoning":
      return <Thought part={part} />;
    case "tool_call":
      return <ToolRow part={part} model={model} busy={busy} compact={compact} />;
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

/** Terminal colour codes, for notices stored before the engine stripped them. */
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g;

/**
 * A user-role message Genie wrote (a background job finished, a resume note): one quiet
 * line with the details folded away, never a chat bubble in the user's name (ticket 44).
 */
function Notice({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const body = text.replace(ANSI, "").replace(/<\/?system-update>/g, "").trim();
  const nl = body.indexOf("\n");
  const head = nl < 0 ? body : body.slice(0, nl);
  const rest = nl < 0 ? "" : body.slice(nl + 1).trim();
  return (
    <div className="divider notice" role="note">
      <span className="notice__head">
        {head}
        {rest && (
          <>
            {" · "}
            <button className="linklike" onClick={() => setOpen((o) => !o)}>
              {open ? t("chat.hideDetails") : t("chat.showDetails")}
            </button>
          </>
        )}
      </span>
      {open && rest && <pre className="divider__summary notice__body">{rest}</pre>}
    </div>
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

function ToolRow({ part, model, busy, compact, nested }: { part: PartView; model: SessionModel; busy: boolean; compact: boolean; nested?: boolean }) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState(false);
  const [moving, setMoving] = useState(false);
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
  // Running only while the turn is live: a call whose result was compacted away or whose
  // turn was interrupted has no outcome but is not running.
  const running = busy && !tool?.outcome && (part.streaming || !!tool);
  const status = tool?.outcome === "blocked" ? t("tool.blocked") : tool?.outcome === "declined" ? t("tool.declined") : tool?.outcome === "error" ? t("tool.failed") : null;
  const out = headTail(resultText(content));

  void compact;
  if (sum.memory)
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
  if (sum.artifact)
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
    <div className={`tool ${nested ? "tool--nested" : ""} ${open ? "is-open" : ""}`}>
      <button className="tool__head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <Icon name={sum.icon} size={14} />
        <span className={`tool__title ${running ? "is-running" : ""}`}>
          <InlineCode text={sum.title} />
        </span>
        {sum.server && <Badge>{sum.server}</Badge>}
        {status && <Badge tone={tool?.outcome === "error" ? "danger" : "warning"}>{status}</Badge>}
        {running && tool?.progress?.message && <span className="muted tool__progress">{tool.progress.message}</span>}
        <span className="tool__chev" aria-hidden>
          <Icon name={open ? "chevronDown" : "chevronRight"} size={12} />
        </span>
      </button>
      {running && name === "shell" && call && (
        // A server or watcher never exits: let the user hand it to a background job so the
        // turn continues (ticket 43). The daemon also does this when it sees a listening port.
        <button
          className="linklike tool__bg"
          disabled={moving}
          title={t("tool.moveToBackgroundHint")}
          onClick={() => {
            setMoving(true);
            app.client.session
              .background_tool({ session_id: model.sessionId, call_id: call.call_id })
              .catch((e) => {
                setMoving(false);
                app.toast({ title: app.describeError(e), tone: "danger" });
              });
          }}
        >
          {t("tool.moveToBackground")}
        </button>
      )}
      {tool?.error && <div className="tool__error">{tool.error.message}</div>}
      {name === "preview_start" && tool?.outcome === "ok" && (
        <ServerCard sessionId={model.sessionId} name={str((input as Record<string, unknown> | undefined)?.name) || serverNameFrom(resultText(content))} urlHint={urlFrom(resultText(content))} />
      )}
      {sum.images && sum.images.length > 0 && <GeneratedImages images={sum.images} prompt={typeof (input as { prompt?: unknown })?.prompt === "string" ? (input as { prompt: string }).prompt : ""} />}
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

const str = (v: unknown) => (typeof v === "string" ? v : "");
/** "`web`: running at http://localhost:5173 (…)" → "web". */
const serverNameFrom = (text: string) => /^`([^`]+)`/.exec(text)?.[1] ?? "";
const urlFrom = (text: string) => / at (https?:\/\/[^\s,)]+)/.exec(text)?.[1] ?? null;

/**
 * The card under a `preview_start` row (ticket 47): the server's live state from
 * `preview.list` / `preview.changed`, with Open (Browser tab) and Stop, or Start again
 * once it has stopped.
 */
function ServerCard({ sessionId, name, urlHint }: { sessionId: string; name: string; urlHint: string | null }) {
  const app = useAppStatic();
  useEmitter(app.devServers);
  useEffect(() => app.devServers.ensure(sessionId), [app, sessionId]);
  const [busy, setBusy] = useState(false);
  if (!name) return null;
  const s = app.devServers.server(sessionId, name);
  const url = s?.url ?? urlHint;
  const state = s?.state ?? "exited";
  const label = { starting: t("preview.starting"), running: t("preview.running"), exited: t("preview.stopped"), failed: t("preview.failed"), unknown: t("preview.stopped") }[state];
  const act = (f: () => Promise<unknown>) => {
    setBusy(true);
    f()
      .catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }))
      .finally(() => setBusy(false));
  };
  return (
    <div className={`server-card is-${state}`}>
      <span className="server-card__dot" aria-hidden />
      <span className="server-card__main">
        <span className="server-card__name">{name}</span>
        <span className="server-card__meta">
          {label}
          {url && state !== "exited" && state !== "failed" && <> · {url.replace(/^https?:\/\//, "")}</>}
        </span>
      </span>
      {state === "running" || state === "starting" ? (
        <>
          {url && (
            <Button size="sm" icon="globe" disabled={state !== "running"} onClick={() => app.openInBrowser(sessionId, url)}>
              {t("common.open")}
            </Button>
          )}
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(() => app.devServers.stop(sessionId, name))}>
            {t("preview.stop")}
          </Button>
        </>
      ) : (
        <Button size="sm" icon="play" disabled={busy} onClick={() => act(() => app.devServers.start(sessionId, name))}>
          {t("preview.start")}
        </Button>
      )}
    </div>
  );
}

function Media({ part }: { part: Extract<Part, { type: "media" }> }) {
  return <BlobImage blobId={part.blob_id} mime={part.mime} name={part.name ?? undefined} />;
}

/** A stored blob shown as an image (a badge while loading, or when it isn't an image). */
function BlobImage({ blobId, mime, name, title }: { blobId: string; mime: string; name?: string; title?: string }) {
  const app = useAppStatic();
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!mime.startsWith("image/")) return;
    let u: string | null = null;
    let live = true;
    readBlob(app.client, blobId, 32 * 1024 * 1024)
      .then((bytes) => {
        if (!live) return;
        u = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
        setUrl(u);
      })
      .catch(() => {});
    return () => {
      live = false;
      if (u) URL.revokeObjectURL(u);
    };
  }, [app, blobId, mime]);
  if (url) return <img className="media" src={url} alt={name ?? ""} title={title} />;
  return (
    <Badge>
      <Icon name="file" /> {name ?? mime}
    </Badge>
  );
}

/** `image_generate` results: the images, each with a save link. */
function GeneratedImages({ images, prompt }: { images: GeneratedImage[]; prompt: string }) {
  return (
    <div className="tool__images">
      {images.map((img) => (
        <figure key={img.blob_id} className="tool__image">
          <BlobImage blobId={img.blob_id} mime={img.mime} name={prompt} title={img.revised_prompt ?? prompt} />
          <SaveBlob blobId={img.blob_id} mime={img.mime} />
        </figure>
      ))}
    </div>
  );
}

function SaveBlob({ blobId, mime }: { blobId: string; mime: string }) {
  const app = useAppStatic();
  const ext = mime.split("/")[1]?.replace("jpeg", "jpg") ?? "png";
  return (
    <button
      className="linklike tool__save"
      onClick={() => {
        readBlob(app.client, blobId, 32 * 1024 * 1024)
          .then((bytes) => {
            const u = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
            const a = document.createElement("a");
            a.href = u;
            a.download = `genie-image-${blobId.slice(-8)}.${ext}`;
            a.click();
            setTimeout(() => URL.revokeObjectURL(u), 1000);
          })
          .catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
      }}
    >
      <Icon name="download" size={12} /> {t("tool.saveImage")}
    </button>
  );
}

/**
 * What Genie is doing while a turn runs, below the last message: an animated mark plus a
 * shimmering label ("Thinking", "Running a command", "Working on it"). While reply text is
 * streaming only the mark shows. Hidden when idle or waiting on an approval card.
 */
export function TurnIndicator({ store }: { store: SessionStore }) {
  const m = store.model;
  if (!store.busy || m.status === "awaiting_ask" || m.status === "paused") return null;
  const label = turnLabel(m);
  return (
    // While reply text streams, tuck the mark into the reply's reserved action row.
    <div className={`turn ${label ? "" : "turn--tuck"}`} role="status" aria-label={label || t("chat.statusRunning")}>
      <span className="turn__mark" aria-hidden>
        <GenieMark size={20} state="thinking" />
      </span>
      {label && <span className="turn__label">{label}</span>}
    </div>
  );
}

/** The indicator's label (pure, for tests): "" means "mark only" (reply text is streaming). */
export function turnLabel(m: SessionModel): string {
  if (m.status === "compacting") return t("chat.statusCompacting");
  const running = [...m.tools.values()].filter((x) => !x.outcome);
  if (running.length) return toolActivity(running[running.length - 1].name);
  const last = m.messages[m.messages.length - 1];
  const live = last?.role === "assistant" ? last.parts.find((p) => p.streaming) : undefined;
  if (live?.kind === "reasoning") return t("chat.thinkingShort");
  if (live?.kind === "text") return "";
  if (live?.kind === "tool_call") return t("chat.preparingTool");
  return t("chat.workingOnIt");
}

type Row = { key: string; kind: "msg"; msg: MessageView } | { key: string; kind: "tools"; parts: PartView[] };
type Chunk = { kind: "part"; part: PartView } | { kind: "tools"; key: string; parts: PartView[] };

/**
 * Is this part something the transcript draws? Tool results render inside their call, and
 * reasoning only while it streams or when there is text to read: withheld ("redacted") or
 * empty thinking draws nothing, so the tool steps around it fold into one activity line.
 */
function visiblePart(p: PartView): boolean {
  if (p.kind === "text") return p.streaming || p.text.trim().length > 0;
  if (p.kind === "reasoning") return p.streaming || (!(p.final?.type === "reasoning" && p.final.redacted) && p.text.trim().length > 0);
  return p.kind === "tool_call" || p.kind === "media" || p.kind === "compaction";
}

/** A plain tool call that can fold into a group (memory chips and artifact cards stand alone). */
function groupable(p: PartView): boolean {
  if (p.kind !== "tool_call") return false;
  const name = p.final?.type === "tool_call" ? p.final.name : "";
  return !name.startsWith("memory_") && !name.startsWith("artifact_") && name !== "preview_start";
}

/**
 * Transcript rows (pure, for tests): messages with nothing to draw (tool-result-only steps)
 * are dropped, and runs of tool-only assistant steps fold into one "Read 4 files" row.
 */
export function buildRows(messages: MessageView[]): Row[] {
  const rows: Row[] = [];
  for (const m of messages) {
    const vis = m.parts.filter(visiblePart);
    if (m.role !== "user" && !vis.length) continue;
    if (m.role !== "user" && vis.every(groupable)) {
      const last = rows[rows.length - 1];
      if (last?.kind === "tools") last.parts = [...last.parts, ...vis];
      else rows.push({ key: `tools:${m.id}`, kind: "tools", parts: vis });
      continue;
    }
    rows.push({ key: m.id, kind: "msg", msg: m });
  }
  return rows;
}

/**
 * The row showing a message: its own row, the tool group it folded into, or (for a message
 * the transcript doesn't draw) the closest drawn message before it. -1 when there is none.
 */
export function rowIndexOf(messages: MessageView[], rows: Row[], messageId: string): number {
  const at = messages.findIndex((m) => m.id === messageId);
  for (let i = at; i >= 0; i--) {
    const m = messages[i];
    const ids = new Set(m.parts.map((p) => p.id));
    const r = rows.findIndex((row) => (row.kind === "msg" ? row.msg.id === m.id : row.key === `tools:${m.id}` || row.parts.some((p) => ids.has(p.id))));
    if (r >= 0) return r;
  }
  return -1;
}

/** Within one message: consecutive tool calls fold into a group, everything else as is. */
function chunkParts(parts: PartView[], _model: SessionModel): Chunk[] {
  const out: Chunk[] = [];
  for (const p of parts) {
    if (!visiblePart(p)) continue;
    const last = out[out.length - 1];
    if (groupable(p)) {
      if (last?.kind === "tools") last.parts.push(p);
      else out.push({ kind: "tools", key: `g:${p.id}`, parts: [p] });
    } else out.push({ kind: "part", part: p });
  }
  return out;
}

/** One tool call shows as its own line; several fold into "Read 4 files, ran a command ›". */
function ToolGroup({ parts, model, busy, compact }: { parts: PartView[]; model: SessionModel; busy: boolean; compact: boolean }) {
  const [open, setOpen] = useState(false);
  if (parts.length === 1) return <ToolRow part={parts[0]} model={model} busy={busy} compact={compact} />;
  const names = parts.map((p) => (p.final?.type === "tool_call" ? p.final.name : "tool"));
  const outcomes = parts.map((p) => (p.final?.type === "tool_call" ? model.tools.get(p.final.call_id)?.outcome : null));
  const running = busy && outcomes.some((o) => !o);
  const failed = outcomes.filter((o) => o === "error").length;
  return (
    <div className={`toolgroup ${open ? "is-open" : ""}`}>
      <button className="tool__head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <Icon name="layers" size={14} />
        <span className={`tool__title ${running ? "is-running" : ""}`}>{toolGroupSummary(names)}</span>
        {failed > 0 && <Badge tone="danger">{t("tool.failedCount", { n: failed })}</Badge>}
        <span className="tool__chev" aria-hidden>
          <Icon name={open ? "chevronDown" : "chevronRight"} size={12} />
        </span>
      </button>
      {open && (
        <div className="toolgroup__list">
          {parts.map((p) => (
            <ToolRow key={p.id} part={p} model={model} busy={busy} compact={compact} nested />
          ))}
        </div>
      )}
    </div>
  );
}

/** Tool titles mark commands and paths with backticks; show those as code, the rest as text. */
function InlineCode({ text }: { text: string }) {
  const bits = text.split("`");
  return <>{bits.map((b, i) => (i % 2 ? <code key={i}>{b}</code> : b))}</>;
}
