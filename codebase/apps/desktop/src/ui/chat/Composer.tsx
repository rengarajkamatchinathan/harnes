// Composer (05 §2.3): Enter sends, Shift+Enter newline; while a turn runs Enter steers and
// Alt+Enter queues (SA-06); attachments via blob.put; model + effort pickers; "+" menu;
// context meter ring (D32); queued items as chips.
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { Delivery, InputPart, McpServerInfo, ModelInfo, SkillInfo } from "@genie/protocol";
import { Button, Chip, Dialog, IconButton, Menu, Ring, Select, Switch } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import { formatBytes, guessMime, rejectReason, uploadBlob, type Attachment } from "../../state/attachments";
import type { SessionStore } from "../../state/session";
import { useApp } from "../context";
import { McpPromptPicker, McpResourcePicker, type McpResource } from "./McpPickers";

export interface ComposerProps {
  store: SessionStore | null;
  /** New chat: create the session first, then send. */
  onCreate?: () => Promise<SessionStore>;
  readOnly?: boolean;
}

/** Which key does what (pure, for tests). */
export function keyAction(e: { key: string; shiftKey: boolean; altKey: boolean; isComposing?: boolean }, busy: boolean): "send" | "queue" | "newline" | null {
  if (e.key !== "Enter" || e.isComposing) return null;
  if (e.shiftKey) return "newline";
  if (e.altKey) return busy ? "queue" : "newline";
  return "send";
}

export function groupModels(models: ModelInfo[]) {
  return models.map((m) => ({ value: m.id, label: m.name || m.id, group: m.provider }));
}

let keySeq = 0;

export function Composer({ store, onCreate, readOnly }: ComposerProps) {
  const app = useApp();
  const [text, setText] = useState("");
  const [atts, setAtts] = useState<Attachment[]>([]);
  const [drag, setDrag] = useState(false);
  const [sending, setSending] = useState(false);
  const [connectors, setConnectors] = useState(false);
  const [skillPick, setSkillPick] = useState(false);
  const [mcpPick, setMcpPick] = useState<"resource" | "prompt" | null>(null);
  const [codeNotice, setCodeNotice] = useState(false);
  // Per-chat provider tools (06 §8, D35): web search on, code execution off by default.
  const webSearch = store?.model.info?.web_search ?? true;
  const codeExec = store?.model.info?.code_execution ?? false;
  const toggle = async (key: "web_search" | "code_execution", value: boolean) => {
    if (!store) return;
    try {
      await store.setToggle(key, value);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };

  /** An MCP resource attaches like a file: the daemon reads it into a blob. */
  const attachResource = async (r: McpResource) => {
    try {
      const got = await app.client.mcp.resource.read({ name: r.server, uri: r.uri });
      setAtts((x) => [...x, { key: `mcp${++keySeq}`, name: r.name, mime: got.mime || r.mime || "text/plain", size: 0, state: "ready", progress: 1, blobId: got.blob_id }]);
      setMcpPick(null);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };
  const fileRef = useRef<HTMLInputElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const model = store?.model;
  const busy = store?.busy ?? false;
  const info = model?.info;
  const currentModel = info?.model ?? app.ui.model ?? app.models[0]?.id ?? "";
  const modelInfo = app.models.find((m) => m.id === currentModel);
  const effortLevels = modelInfo?.effort_levels ?? [];
  const usage = model?.usage;
  const ctx = usage && usage.context_window ? usage.context_tokens / usage.context_window : 0;

  // Autosize.
  useEffect(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 280)}px`;
  }, [text]);

  const addFiles = (files: FileList | File[]) => {
    const list = [...files];
    let count = atts.length;
    for (const f of list) {
      const reason = rejectReason(f, count);
      if (reason) {
        app.toast({ title: reason, tone: "danger" });
        continue;
      }
      count++;
      const key = `att${++keySeq}`;
      const att: Attachment = { key, name: f.name, mime: guessMime(f.name, f.type), size: f.size, state: "uploading", progress: 0 };
      setAtts((a) => [...a, att]);
      const update = (patch: Partial<Attachment>) => setAtts((a) => a.map((x) => (x.key === key ? { ...x, ...patch } : x)));
      uploadBlob(app.client, f, (p) => update({ progress: p }))
        .then((blobId) => update({ blobId, state: "ready", progress: 1 }))
        .catch((e) => update({ state: "error", error: app.describeError(e) }));
    }
  };

  const submit = async (delivery: Delivery) => {
    const body = text.trim();
    const ready = atts.filter((a) => a.state === "ready" && a.blobId);
    if ((!body && !ready.length) || sending || atts.some((a) => a.state === "uploading")) return;
    const parts: InputPart[] = [];
    const skill = /^\/([\w:-]+)\s*/.exec(body);
    if (skill && !skill[1].includes(":")) parts.push({ type: "skill", id: skill[1] });
    const rest = skill && !skill[1].includes(":") ? body.slice(skill[0].length) : body;
    if (rest) parts.push({ type: "text", text: rest });
    for (const a of ready) parts.push({ type: "attachment", blob_id: a.blobId!, mime: a.mime, name: a.name });
    setSending(true);
    try {
      const s = store ?? (await onCreate?.());
      if (!s) return;
      await s.send(parts, delivery);
      setText("");
      setAtts([]);
    } catch (e) {
      const msg = e instanceof Error && e.message === "no_provider_configured" ? t("chat.emptyNoProvider") : app.describeError(e);
      app.toast({ title: msg, tone: "danger" });
    } finally {
      setSending(false);
    }
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    const a = keyAction({ key: e.key, shiftKey: e.shiftKey, altKey: e.altKey, isComposing: e.nativeEvent.isComposing }, busy);
    if (a === "send" || a === "queue") {
      e.preventDefault();
      void submit(a === "queue" ? "queue" : "steer");
    } else if (e.key === "Escape" && busy) {
      void store?.interrupt();
    }
  };

  const setModel = (id: string) => {
    app.setUi({ model: id });
    if (store) void app.client.session.update({ session_id: store.id, model: id }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  };
  const setEffort = (effort: string) => {
    app.setUi({ effort });
    if (store) void app.client.session.update({ session_id: store.id, effort }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
  };

  const modelOptions = useMemo(() => groupModels(app.models), [app.models]);
  const tooBig = (a: Attachment) => !!modelInfo?.context_window && a.size / 4 > modelInfo.context_window;

  if (readOnly) return <div className="composer composer--readonly muted">{t("composer.readOnly")}</div>;

  return (
    <div
      className={`composer ${drag ? "composer--drag" : ""}`}
      onDragOver={(e) => {
        e.preventDefault();
        setDrag(true);
      }}
      onDragLeave={() => setDrag(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDrag(false);
        if (e.dataTransfer.files.length) addFiles(e.dataTransfer.files);
      }}
    >
      {model && model.inbox.length > 0 && (
        <div className="row wrap composer__queue" aria-label={t("composer.queued")}>
          {model.inbox.map((q) => (
            <Chip key={q.id} icon="layers" title={`${q.delivery}: ${q.preview}`} removeLabel={t("composer.removeQueued")} onRemove={() => void store?.removeQueued(q.id)}>
              {q.preview}
            </Chip>
          ))}
        </div>
      )}
      {atts.length > 0 && (
        <div className="row wrap composer__atts">
          {atts.map((a) => (
            <Chip
              key={a.key}
              icon="paperclip"
              title={a.error ?? (tooBig(a) ? t("composer.tooBig") : undefined)}
              onRemove={() => setAtts((x) => x.filter((y) => y.key !== a.key))}
              removeLabel={t("common.delete")}
            >
              {a.name} · {a.state === "uploading" ? `${Math.round(a.progress * 100)}%` : a.key.startsWith("mcp") ? "MCP" : formatBytes(a.size)}
              {(a.state === "error" || tooBig(a)) && " ⚠"}
            </Chip>
          ))}
        </div>
      )}
      <textarea
        ref={taRef}
        className="composer__input"
        rows={1}
        value={text}
        placeholder={drag ? t("composer.dropHere") : busy ? t("composer.placeholderBusy") : t("composer.placeholder")}
        aria-label={t("composer.placeholder")}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
        onPaste={(e) => {
          if (e.clipboardData.files.length) {
            e.preventDefault();
            addFiles(e.clipboardData.files);
          }
        }}
      />
      <div className="composer__bar">
        <Menu
          label={t("composer.plus")}
          placement="up"
          trigger={(p) => <IconButton icon="plus" label={t("composer.plus")} {...p} />}
          items={[
            { id: "attach", label: t("composer.attach"), icon: "paperclip", onSelect: () => fileRef.current?.click() },
            { id: "resource", label: t("composer.mcpResource"), icon: "layers", onSelect: () => setMcpPick("resource") },
            { id: "skills", label: t("composer.skills"), icon: "sparkle", onSelect: () => setSkillPick(true) },
            { id: "prompt", label: t("composer.mcpPrompt"), icon: "layers", onSelect: () => setMcpPick("prompt") },
            { id: "connectors", label: t("composer.connectors"), icon: "layers", disabled: !store, onSelect: () => setConnectors(true) },
            { id: "web", label: webSearch ? t("composer.webSearchOn") : t("composer.webSearchOff"), icon: "globe", disabled: !store, onSelect: () => void toggle("web_search", !webSearch) },
            { id: "code", label: codeExec ? t("composer.codeExecOn") : t("composer.codeExecOff"), icon: "code", disabled: !store, onSelect: () => (codeExec ? void toggle("code_execution", false) : app.ui.codeExecNoticeSeen ? void toggle("code_execution", true) : setCodeNotice(true)) },
            { id: "browser", label: t("composer.browser"), icon: "globe", onSelect: () => app.setUi({ panelOpen: true, panelTab: "browser" }) },
          ]}
        />
        <input ref={fileRef} type="file" multiple hidden onChange={(e) => e.target.files && addFiles(e.target.files)} />
        <Select className="composer__model" aria-label={t("composer.model")} value={currentModel} options={modelOptions.length ? modelOptions : [{ value: "", label: "—" }]} onChange={setModel} />
        {effortLevels.length > 0 && (
          <Select className="composer__effort" aria-label={t("composer.effort")} value={info?.effort ?? app.ui.effort ?? effortLevels[0]} options={effortLevels.map((e) => ({ value: e, label: e }))} onChange={setEffort} />
        )}
        <span className="grow" />
        {usage && (
          <span
            className="composer__ring"
            title={t("composer.context", { used: formatTokens(usage.context_tokens), window: formatTokens(usage.context_window), cost: formatCost(usage.cost.usd) })}
          >
            <Ring value={ctx} warn={ctx > 0.8} label={`${Math.round(ctx * 100)}%`} />
          </span>
        )}
        {busy && (
          <Button variant="secondary" icon="stop" onClick={() => void store?.interrupt()} aria-label={t("composer.stop")}>
            {t("composer.stop")}
          </Button>
        )}
        {(!busy || text.trim()) && (
          <IconButton icon="send" variant="primary" label={t("composer.send")} disabled={sending || (!text.trim() && !atts.some((a) => a.state === "ready"))} onClick={() => void submit("steer")} />
        )}
      </div>
      {connectors && store && <ConnectorToggles sessionId={store.id} onClose={() => setConnectors(false)} />}
      {codeNotice && (
        <Dialog
          open
          title={t("composer.codeExecNoticeTitle")}
          onClose={() => setCodeNotice(false)}
          footer={
            <>
              <Button onClick={() => setCodeNotice(false)}>{t("common.cancel")}</Button>
              <Button
                variant="primary"
                onClick={() => {
                  setCodeNotice(false);
                  app.setUi({ codeExecNoticeSeen: true });
                  void toggle("code_execution", true);
                }}
              >
                {t("composer.codeExecEnable")}
              </Button>
            </>
          }
        >
          <p>{t("composer.codeExecNotice")}</p>
        </Dialog>
      )}
      {mcpPick === "resource" && <McpResourcePicker onPick={attachResource} onClose={() => setMcpPick(null)} />}
      {mcpPick === "prompt" && (
        <McpPromptPicker
          onInsert={(cmd) => {
            setText((x) => (x.trim() ? `${cmd} ${x}` : `${cmd} `));
            setMcpPick(null);
            taRef.current?.focus();
          }}
          onClose={() => setMcpPick(null)}
        />
      )}
      {skillPick && (
        <SkillPicker
          onPick={(s) => {
            setText((x) => `/${s.id} ${x}`);
            setSkillPick(false);
            taRef.current?.focus();
          }}
          onClose={() => setSkillPick(false)}
        />
      )}
    </div>
  );
}

/** Per-chat server toggles (D30) via `mcp.set_enabled {session_id}`. */
function ConnectorToggles({ sessionId, onClose }: { sessionId: string; onClose: () => void }) {
  const app = useApp();
  const [servers, setServers] = useState<McpServerInfo[] | null>(null);
  const [off, setOff] = useState<Set<string>>(new Set());
  useEffect(() => {
    app.client.mcp
      .list({})
      .then((r) => setServers(r.servers))
      .catch(() => setServers([]));
  }, [app]);
  return (
    <Dialog open title={t("composer.connectors")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      {!servers ? (
        t("common.loading")
      ) : servers.length === 0 ? (
        <p className="muted">No MCP servers configured.</p>
      ) : (
        <div className="stack">
          {servers.map((s) => (
            <Switch
              key={s.name}
              checked={!off.has(s.name) && s.status !== "disabled"}
              label={`${s.name} · ${s.tools} tools`}
              onChange={(v) => {
                const next = new Set(off);
                if (v) next.delete(s.name);
                else next.add(s.name);
                setOff(next);
                void app.client.mcp.set_enabled({ name: s.name, enabled: v, session_id: sessionId }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
              }}
            />
          ))}
        </div>
      )}
    </Dialog>
  );
}

function SkillPicker({ onPick, onClose }: { onPick: (s: SkillInfo) => void; onClose: () => void }) {
  const app = useApp();
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  useEffect(() => {
    app.client.skill
      .list({})
      .then((r) => setSkills(r.skills.filter((s) => s.enabled)))
      .catch(() => setSkills([]));
  }, [app]);
  return (
    <Dialog open title={t("composer.skills")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      {!skills ? (
        t("common.loading")
      ) : (
        <div className="stack">
          {skills.map((s) => (
            <button key={s.id} className="list-button" onClick={() => onPick(s)}>
              <strong>/{s.id}</strong> <span className="muted">{s.description}</span>
            </button>
          ))}
          {skills.length === 0 && <p className="muted">No skills enabled.</p>}
        </div>
      )}
    </Dialog>
  );
}
