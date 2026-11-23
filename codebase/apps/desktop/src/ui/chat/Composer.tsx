// Composer (05 §2.3): Enter sends, Shift+Enter newline; while a turn runs Enter steers and
// Alt+Enter queues (SA-06); attachments via blob.put; model + effort pickers; "+" menu;
// context meter ring (D32); queued items as chips; the `/` menu (D58).
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import type { CommandInfo, Delivery, InputPart, McpServerInfo, ModelInfo } from "@genie/protocol";
import { Button, Chip, Dialog, Icon, IconButton, Menu, Ring, Switch } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import { formatBytes, guessMime, rejectReason, uploadBlob, type Attachment } from "../../state/attachments";
import type { SessionStore } from "../../state/session";
import { useApp } from "../context";
import { isOffEffort } from "../../state/modelFilter";
import { providerMeta } from "../settings/ProvidersSection";
import { RewindDialog } from "../code/CodeExtras";
import { raceBook, startRace } from "../../state/race";
import { CompareChip, CompareToggle, useRaceModels } from "./Compare";
import { checksFor, sendGate, type BudgetCheck } from "../../state/budget";
import { BudgetNotice, checkMessage, useBudgetChecks } from "../BudgetGuard";
import { useDictation } from "./dictation";
import { McpPromptPicker, McpResourcePicker, type McpResource } from "./McpPickers";
import { findCommand, hintAfter, needsArgs, parseSlash, pushRecent, rankCommands, slashQuery } from "./slash";
import { ChoiceDialog, CommandPicker, HelpDialog, SlashMenu, runClientCommand, useCommands, type CommandDialog } from "./SlashMenu";

export interface ComposerProps {
  store: SessionStore | null;
  /** New chat: create the session first, then send. */
  onCreate?: () => Promise<SessionStore | null>;
  readOnly?: boolean;
  /** "hero": empty-state box with the model picker inside; "reply": one row, pickers below. */
  layout?: "hero" | "reply";
  placeholder?: string;
  /** Prefill the box (e.g. an "Ideas for you" row); a new `n` re-applies the text. */
  seed?: { text: string; n: number };
  /** Extra controls after the "+" menu in the hero toolbar (e.g. the project picker). */
  leading?: ReactNode;
}

/** Which key does what (pure, for tests). */
export function keyAction(e: { key: string; shiftKey: boolean; altKey: boolean; isComposing?: boolean }, busy: boolean): "send" | "queue" | "newline" | null {
  if (e.key !== "Enter" || e.isComposing) return null;
  if (e.shiftKey) return "newline";
  if (e.altKey) return busy ? "queue" : "newline";
  return "send";
}

function providerName(id: string): string {
  return providerMeta({ id, kind: id }).name;
}

export function groupModels(models: ModelInfo[]) {
  return models.map((m) => ({ value: m.id, label: m.name || m.id, group: m.provider }));
}

let keySeq = 0;

/**
 * When `key` changes, glide the element from its previous width to its new one (FLIP with the
 * Web Animations API), so neighbours slide instead of jumping. Skipped under reduced motion.
 */
function useWidthGlide(ref: RefObject<HTMLElement | null>, key: unknown, ms = 260) {
  const last = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const next = el.getBoundingClientRect().width;
    const prev = last.current;
    last.current = next;
    if (prev == null || Math.abs(prev - next) < 1 || typeof el.animate !== "function") return;
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const overflow = el.style.overflow;
    el.style.overflow = "hidden";
    const anim = el.animate([{ width: `${prev}px` }, { width: `${next}px` }], { duration: ms, easing: "cubic-bezier(0.16, 1, 0.3, 1)" });
    const done = () => {
      el.style.overflow = overflow;
      if (ref.current) last.current = ref.current.getBoundingClientRect().width;
    };
    anim.onfinish = done;
    anim.oncancel = done;
    return () => anim.cancel();
  }, [key, ref, ms]);
}

/** Model + effort as quiet text buttons ("Genie Small  Medium ⌄"), like the reference. */
export function ModelPicker({ store }: { store: SessionStore | null }) {
  const app = useApp();
  const info = store?.model.info;
  const currentModel = info?.model ?? app.preferredModel() ?? "";
  const modelInfo = app.models.find((m) => m.id === currentModel);
  // "Off" isn't offered: thinking effort is picked from the real levels (low/medium/high…).
  const effortLevels = (modelInfo?.effort_levels ?? []).filter((e) => !isOffEffort(e));
  const picked = info?.effort ?? app.ui.effort ?? null;
  const effort = picked && effortLevels.includes(picked) ? picked : (effortLevels[0] ?? null);
  const fail = (e: unknown) => app.toast({ title: app.describeError(e), tone: "danger" });
  const setModel = (id: string) => {
    app.setUi({ model: id });
    if (store) void app.client.session.update({ session_id: store.id, model: id }).catch(fail);
  };
  const setEffort = (e: string) => {
    app.setUi({ effort: e });
    if (store) void app.client.session.update({ session_id: store.id, effort: e }).catch(fail);
  };
  // Switching model can change the name's width and add/remove the effort picker: glide it.
  // (Hooks run before the early return below.)
  const boxRef = useRef<HTMLSpanElement>(null);
  useWidthGlide(boxRef, `${currentModel}|${effortLevels.length > 0}|${effort}`);
  // Only models from connected providers; a session keeps showing the model it already uses.
  const usable = app.usableModels;
  if (!currentModel)
    return (
      <span className="modelpick">
        <button className="textbtn" onClick={() => app.openDeepLink("genie://settings/providers")}>
          <Icon name="plug" size={14} /> {t("composer.connectProvider")}
        </button>
      </span>
    );
  const multi = new Set(usable.map((x) => x.provider)).size > 1;
  return (
    <span className="modelpick" ref={boxRef}>
      <Menu
        label={t("composer.model")}
        placement="up"
        align="right"
        className="modelpick__list"
        items={usable.map((m) => ({
          id: m.id,
          label: m.name || m.id,
          hint: m.context_window ? t("composer.modelContext", { n: formatTokens(m.context_window) }) : undefined,
          group: multi ? providerName(m.provider) : undefined,
          checked: m.id === currentModel,
          onSelect: () => setModel(m.id),
        }))}
        trigger={(p) => (
          <button className="modelbtn" aria-label={t("composer.modelChoose", { name: modelInfo?.name || currentModel || "—" })} {...p}>
            <Icon key={`spark-${currentModel}`} name="sparkle" size={14} />
            {/* Keyed on the model so a new choice re-runs the settle-in animation. */}
            <span key={currentModel} className="modelbtn__name">
              {modelInfo?.name || currentModel || "—"}
            </span>
            <Icon name="chevronDown" size={12} />
          </button>
        )}
      />
      {effortLevels.length > 0 && (
        <Menu
          label={t("composer.effort")}
          placement="up"
          align="right"
          items={effortLevels.map((e) => ({ id: e, label: e[0].toUpperCase() + e.slice(1), checked: e === effort, onSelect: () => setEffort(e) }))}
          trigger={(p) => (
            <button key={currentModel} className="textbtn textbtn--muted modelpick__effort" aria-label={t("composer.effort")} {...p}>
              {effort ? effort[0].toUpperCase() + effort.slice(1) : ""}
              <Icon name="chevronDown" size={12} />
            </button>
          )}
        />
      )}
    </span>
  );
}

/** Context-window meter for the composer footer (D32); hidden until the session has usage. */
export function ContextRing({ store }: { store: SessionStore | null }) {
  const usage = store?.model.usage;
  if (!usage || !usage.context_window) return null;
  const ctx = usage.context_tokens / usage.context_window;
  return (
    <span className="composer__ring" title={t("composer.context", { used: formatTokens(usage.context_tokens), window: formatTokens(usage.context_window), cost: formatCost(usage.cost.usd) })}>
      <Ring value={ctx} size={16} warn={ctx > 0.8} label={`${Math.round(ctx * 100)}%`} />
    </span>
  );
}

export function Composer({ store, onCreate, readOnly, layout = "reply", placeholder, seed, leading }: ComposerProps) {
  const app = useApp();
  const [text, setText] = useState("");
  useEffect(() => {
    if (!seed) return;
    setText(seed.text);
    taRef.current?.focus();
  }, [seed]);
  const [atts, setAtts] = useState<Attachment[]>([]);
  const [drag, setDrag] = useState(false);
  const [sending, setSending] = useState(false);
  const [connectors, setConnectors] = useState(false);
  const [skillPick, setSkillPick] = useState(false);
  const [menuSel, setMenuSel] = useState(0);
  /** Esc closed the `/` menu for this text; typing reopens it. */
  const [menuClosedFor, setMenuClosedFor] = useState<string | null>(null);
  const [cmdDialog, setCmdDialog] = useState<CommandDialog | null>(null);
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
  const currentModel = info?.model ?? app.preferredModel() ?? "";
  const modelInfo = app.models.find((m) => m.id === currentModel);
  const kind = info?.kind === "code" || (!info && app.ui.mode === "code") ? "code" : "chat";
  const commands = useCommands(app, store, kind);
  // Model race (Chat only): the next send goes to these models when there are two or more.
  const raceWith = useRaceModels(currentModel);
  const racing = kind === "chat" && raceWith.length > 1;
  // Budget guard: a chat's project budget (Code folders have none) plus the overall one.
  const budgetProject = kind === "chat" ? (info ? info.project_id || null : app.ui.activeProject) : null;
  const budgetChecks = useBudgetChecks(budgetProject);
  const [budgetAsk, setBudgetAsk] = useState<{ delivery: Delivery; override?: string; checks: BudgetCheck[] } | null>(null);
  const hasBudget = !!app.ui.budgets.overall || (!!budgetProject && !!app.ui.budgets.projects[budgetProject]);
  // Refresh month-to-date spend shortly after each turn settles (debounced; only with a budget).
  const wasBusy = useRef(busy);
  useEffect(() => {
    if (wasBusy.current && !busy && hasBudget) app.budgetSpend.refresh(1500);
    wasBusy.current = busy;
  }, [busy, hasBudget, app]);

  // The `/` menu (D58): open while the name is typed at the very start of the prompt.
  const query = slashQuery(text);
  const rows = useMemo(() => (query === null ? [] : rankCommands(commands ?? [], query, app.ui.recentCommands)), [query, commands, app.ui.recentCommands]);
  const menuOpen = query !== null && menuClosedFor !== text;
  useEffect(() => setMenuSel(0), [query]);
  // The picked command as an accent token with its grey argument hint.
  const typed = parseSlash(text);
  const typedCmd = typed && /^\/[^\s]+\s/.test(text) ? findCommand(commands ?? [], typed.name) : undefined;
  const tokenLen = typedCmd ? 1 + (typed?.name.length ?? 0) : 0;
  const mirrorRef = useRef<HTMLDivElement>(null);

  // Voice input: dictates into the box after whatever is already typed.
  const dictation = useDictation(
    (spoken) => setText(spoken),
    (kind, detail) =>
      app.toast({
        title: kind === "unsupported" ? t("composer.voiceUnsupported") : kind === "denied" ? t("composer.voiceDenied") : t("composer.voiceFailed", { error: detail ?? "" }),
        tone: kind === "failed" ? "danger" : "info",
      }),
  );
  const toggleVoice = () => {
    if (dictation.listening) dictation.stop();
    else {
      dictation.start(text);
      taRef.current?.focus();
    }
  };

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

  const remember = (name: string) => app.setUi({ recentCommands: pushRecent(app.ui.recentCommands, name) });

  /** A `client` built-in: Genie's own UI, never a model turn (06 §9.1). */
  const runClient = async (c: CommandInfo, args: string) => {
    remember(c.name);
    setText("");
    try {
      const d = await runClientCommand(app, store, kind, c.name, args);
      if (d) setCmdDialog(d);
    } catch (e) {
      app.toast({ title: app.describeError(e), tone: "danger" });
    }
  };

  /** Enter or click on a menu row: run it, or insert it when a required argument is missing. */
  const pick = (c: CommandInfo) => {
    setMenuClosedFor(null);
    if (needsArgs(c, "")) {
      setText(`/${c.name} `);
      taRef.current?.focus();
      return;
    }
    if (c.route === "client") void runClient(c, "");
    else void submit("steer", `/${c.name}`);
  };

  const submit = async (delivery: Delivery, override?: string, budgetOk = false) => {
    const body = (override ?? text).trim();
    const ready = atts.filter((a) => a.state === "ready" && a.blobId);
    if ((!body && !ready.length) || sending || atts.some((a) => a.state === "uploading")) return;
    if (dictation.listening) dictation.stop();
    const parts: InputPart[] = [];
    const slash = parseSlash(body);
    const cmd = slash ? findCommand(commands ?? [], slash.name) : undefined;
    // Attachments stay in the composer for the next message.
    if (slash && cmd?.route === "client") return void runClient(cmd, slash.args);
    if (store && raceBook.running(store.id)) return void app.toast({ title: t("race.busy"), tone: "info" });
    if (racing && busy) return void app.toast({ title: t("race.busyTurn"), tone: "info" });
    if (slash && cmd?.route === "prompt") {
      parts.push({ type: "command", name: cmd.name, args: slash.args });
      remember(cmd.name);
    } else if (body) {
      // Plain text, and an unknown `/word` too (D58).
      parts.push({ type: "text", text: body });
    }
    for (const a of ready) parts.push({ type: "attachment", blob_id: a.blobId!, mime: a.mime, name: a.name });
    if (!budgetOk && hasBudget) {
      // First send after launch: make sure this month's spend is known before deciding.
      if (!app.budgetSpend.loadedAt) await app.budgetSpend.load();
      const name = app.projects.find((p) => p.project_id === budgetProject)?.name ?? "";
      const gate = sendGate(checksFor(app.ui.budgets, app.budgetSpend, budgetProject, name), currentModel, (m) => app.usableModels.some((x) => x.id === m));
      if (gate.kind === "ask") return void setBudgetAsk({ delivery, override, checks: gate.checks });
      if (gate.kind === "switch" && !racing) {
        try {
          app.setUi({ model: gate.model });
          if (store) await app.client.session.update({ session_id: store.id, model: gate.model });
          app.toast({ title: t("budget.switched", { model: app.models.find((m) => m.id === gate.model)?.name ?? gate.model }), body: checkMessage(gate.check), tone: "info" });
        } catch (e) {
          return void app.toast({ title: app.describeError(e), tone: "danger" });
        }
      }
    }
    setSending(true);
    try {
      const s = store ?? (await onCreate?.());
      if (!s) return;
      if (racing) {
        await startRace(s, parts, raceWith, { setModel: (model) => app.client.session.update({ session_id: s.id, model }), describeError: (e) => app.describeError(e) });
        raceBook.setExtras([]); // one race per pick: the chip said "the next send"
      } else await s.send(parts, delivery);
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
    if (menuOpen && !e.nativeEvent.isComposing) {
      const n = rows.length;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (n) setMenuSel((i) => (i + (e.key === "ArrowDown" ? 1 : -1) + n) % n);
        return;
      }
      if (e.key === "Tab" && n) {
        e.preventDefault();
        setText(`/${rows[menuSel].name} `);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.altKey && n) {
        e.preventDefault();
        pick(rows[menuSel]);
        return;
      }
      if (e.key === "Escape") {
        // Closes the menu; it doesn't stop a running turn.
        e.preventDefault();
        setMenuClosedFor(text);
        return;
      }
    }
    // A picked command waiting for its required argument.
    if (e.key === "Enter" && !e.shiftKey && typedCmd && typed && needsArgs(typedCmd, typed.args)) {
      e.preventDefault();
      return;
    }
    const a = keyAction({ key: e.key, shiftKey: e.shiftKey, altKey: e.altKey, isComposing: e.nativeEvent.isComposing }, busy);
    if (a === "send" || a === "queue") {
      e.preventDefault();
      void submit(a === "queue" ? "queue" : "steer");
    } else if (e.key === "Escape" && busy) {
      void store?.interrupt();
    }
  };

  const tooBig = (a: Attachment) => !!modelInfo?.context_window && a.size / 4 > modelInfo.context_window;

  const hint = typedCmd && typed ? hintAfter(typedCmd, typed.args) : null;
  const textarea = (
    <div className="composer__field">
      {typedCmd && (
        <div ref={mirrorRef} className="composer__input composer__mirror" aria-hidden>
          <span className="slash-token">{text.slice(0, tokenLen)}</span>
          {text.slice(tokenLen)}
          {hint && <span className="slash-hint">{text.endsWith(" ") ? "" : " "}{hint}</span>}
        </div>
      )}
      <textarea
        ref={taRef}
        className={`composer__input ${typedCmd ? "composer__input--token" : ""}`}
        rows={1}
        value={text}
        placeholder={drag ? t("composer.dropHere") : busy ? t("composer.placeholderBusy") : (placeholder ?? t("composer.placeholder"))}
        aria-label={t("composer.placeholder")}
        role={menuOpen ? "combobox" : undefined}
        aria-expanded={menuOpen || undefined}
        aria-activedescendant={menuOpen && rows[menuSel] ? `slash-${rows[menuSel].name}` : undefined}
        onChange={(e) => {
          setText(e.target.value);
          if (menuClosedFor !== null && e.target.value !== menuClosedFor) setMenuClosedFor(null);
        }}
        onKeyDown={onKey}
        onBlur={() => query !== null && setMenuClosedFor(text)}
        onFocus={() => setMenuClosedFor(null)}
        onScroll={(e) => {
          if (mirrorRef.current) mirrorRef.current.scrollTop = e.currentTarget.scrollTop;
        }}
        onPaste={(e) => {
          if (e.clipboardData.files.length) {
            e.preventDefault();
            addFiles(e.clipboardData.files);
          }
        }}
      />
    </div>
  );

  if (readOnly) return <div className="composer composer--readonly muted">{t("composer.readOnly")}</div>;

  return (
    <>
    <BudgetNotice checks={budgetChecks} projectId={budgetProject} />
    <div
      className={`composer composer--${layout} ${drag ? "composer--drag" : ""}`}
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
      {menuOpen && <SlashMenu rows={rows} loading={commands === null} selected={menuSel} onHover={setMenuSel} onPick={pick} />}
      {model && model.inbox.length > 0 && (
        <div className="row wrap composer__queue" aria-label={t("composer.queued")}>
          {model.inbox.map((q) => (
            <Chip key={q.id} icon="layers" title={`${q.delivery}: ${q.preview}`} removeLabel={t("composer.removeQueued")} onRemove={() => void store?.removeQueued(q.id)}>
              {q.preview}
            </Chip>
          ))}
        </div>
      )}
      {racing && (
        <div className="row wrap composer__atts">
          <CompareChip models={raceWith} nameOf={(id) => app.models.find((m) => m.id === id)?.name || id} />
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
      {layout === "hero" && textarea}
      <div className="composer__bar">
        <Menu
          label={t("composer.plus")}
          placement="up"
          trigger={(p) => <IconButton icon="plus" size="sm" label={t("composer.plus")} {...p} />}
          items={[
            { id: "attach", label: t("composer.attach"), icon: "paperclip", onSelect: () => fileRef.current?.click() },
            { id: "resource", label: t("composer.mcpResource"), icon: "layers", keepFocus: true, onSelect: () => setMcpPick("resource") },
            { id: "skills", label: t("composer.skills"), icon: "sparkle", keepFocus: true, onSelect: () => setSkillPick(true) },
            { id: "prompt", label: t("composer.mcpPrompt"), icon: "layers", keepFocus: true, onSelect: () => setMcpPick("prompt") },
            { id: "connectors", label: t("composer.connectors"), icon: "layers", disabled: !store, keepFocus: true, onSelect: () => setConnectors(true) },
            { id: "web", label: webSearch ? t("composer.webSearchOn") : t("composer.webSearchOff"), icon: "globe", disabled: !store, onSelect: () => void toggle("web_search", !webSearch) },
            { id: "code", label: codeExec ? t("composer.codeExecOn") : t("composer.codeExecOff"), icon: "code", disabled: !store, onSelect: () => (codeExec ? void toggle("code_execution", false) : app.ui.codeExecNoticeSeen ? void toggle("code_execution", true) : setCodeNotice(true)) },
            { id: "browser", label: t("composer.browser"), icon: "globe", onSelect: () => app.setUi({ panelOpen: true, panelTab: "browser" }) },
          ]}
        />
        <input ref={fileRef} type="file" multiple hidden onChange={(e) => e.target.files && addFiles(e.target.files)} />
        {layout === "hero" && leading}
        {layout === "hero" ? <span className="grow" /> : textarea}
        {layout === "hero" && kind === "chat" && <CompareToggle store={store} />}
        {layout === "hero" && <ModelPicker store={store} />}
        <IconButton
          icon="mic"
          size="sm"
          className={`composer__voice ${dictation.listening ? "is-listening" : ""}`}
          aria-pressed={dictation.listening}
          label={dictation.listening ? t("composer.voiceStop") : dictation.supported ? t("composer.voice") : t("composer.voiceUnsupported")}
          onClick={toggleVoice}
        />
        {!busy && !text.trim() && !atts.length ? null : busy && !text.trim() ? (
          <IconButton icon="stop" size="sm" variant="secondary" className="composer__send" onClick={() => void store?.interrupt()} label={t("composer.stop")} />
        ) : (
          <IconButton
            icon="arrowUp"
            size="sm"
            variant="primary"
            className="composer__send"
            label={t("composer.send")}
            disabled={sending || (!text.trim() && !atts.some((a) => a.state === "ready"))}
            onClick={() => void submit("steer")}
          />
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
        <CommandPicker
          commands={commands}
          onPick={(c) => {
            setText((x) => `/${c.name} ${x.replace(/^\/\S*\s*/, "")}`);
            setSkillPick(false);
            taRef.current?.focus();
          }}
          onClose={() => setSkillPick(false)}
        />
      )}
      {cmdDialog?.kind === "help" && <HelpDialog commands={commands ?? []} onClose={() => setCmdDialog(null)} />}
      {cmdDialog?.kind === "choice" && <ChoiceDialog title={cmdDialog.title} options={cmdDialog.options} onPick={cmdDialog.onPick} onClose={() => setCmdDialog(null)} />}
      {cmdDialog?.kind === "connectors" && store && <ConnectorToggles sessionId={store.id} onClose={() => setCmdDialog(null)} />}
      {cmdDialog?.kind === "rewind" && store && <RewindDialog sessionId={store.id} message={cmdDialog.message} onClose={() => setCmdDialog(null)} onDone={() => setCmdDialog(null)} />}
      {budgetAsk && (
        <Dialog
          open
          title={t("budget.confirmTitle")}
          onClose={() => setBudgetAsk(null)}
          footer={
            <>
              <Button onClick={() => setBudgetAsk(null)}>{t("common.cancel")}</Button>
              <Button
                variant="primary"
                onClick={() => {
                  const a = budgetAsk;
                  setBudgetAsk(null);
                  void submit(a.delivery, a.override, true);
                }}
              >
                {t("budget.sendAnyway")}
              </Button>
            </>
          }
        >
          {budgetAsk.checks.map((c) => (
            <p key={c.scope}>{checkMessage(c)}</p>
          ))}
          <p>{t("budget.confirmBody")}</p>
        </Dialog>
      )}
    </div>
    </>
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
