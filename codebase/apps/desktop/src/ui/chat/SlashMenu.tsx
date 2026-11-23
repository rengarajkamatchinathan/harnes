// The `/` menu (05 §2.3, 06 §9.1, D58): the engine's `command.list` in one flat list of names,
// recently used first, the description in a tooltip; and the `client`-route built-ins, which
// open Genie's own UI and never start a model turn.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CommandInfo, Mode, SessionKind } from "@genie/protocol";
import { Button, Dialog } from "@genie/ui";
import { formatCost, formatTokens, t } from "../../i18n";
import type { App } from "../../state/app";
import type { SessionStore } from "../../state/session";
import type { MessageView } from "../../state/sessionModel";
import { isOffEffort } from "../../state/modelFilter";

/** `command.list` for the session (or, before one exists, for the mode); reloads on `commands.changed`. */
export function useCommands(app: App, store: SessionStore | null, kind: SessionKind): CommandInfo[] | null {
  const [commands, setCommands] = useState<CommandInfo[] | null>(null);
  const sid = store?.id ?? null;
  const version = app.commandsVersion;
  useEffect(() => {
    let live = true;
    app.client.command
      .list(sid ? { session_id: sid } : { kind })
      .then((r) => live && setCommands(r.commands))
      .catch(() => live && setCommands([]));
    return () => {
      live = false;
    };
  }, [app, sid, kind, version]);
  return commands;
}

/** The popup above the composer. Selection is driven by the composer's keys. */
export function SlashMenu({ rows, loading, selected, onHover, onPick }: { rows: CommandInfo[]; loading: boolean; selected: number; onHover: (i: number) => void; onPick: (c: CommandInfo) => void }) {
  const listRef = useRef<HTMLDivElement>(null);
  const [tipTop, setTipTop] = useState<number | null>(null);
  const current = rows[selected];
  useLayoutEffect(() => {
    const list = listRef.current;
    const row = list?.querySelector<HTMLElement>(`[data-index="${selected}"]`);
    if (!list || !row) return setTipTop(null);
    if (row.offsetTop < list.scrollTop) list.scrollTop = row.offsetTop;
    else if (row.offsetTop + row.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = row.offsetTop + row.offsetHeight - list.clientHeight;
    setTipTop(row.offsetTop - list.scrollTop + list.offsetTop);
  }, [selected, rows]);
  return (
    <div className="slash-menu" role="presentation">
      <div ref={listRef} className="slash-menu__list" role="listbox" aria-label={t("slash.menu")} onScroll={() => setTipTop(null)}>
        {rows.map((c, i) => (
          <div
            key={c.name}
            id={`slash-${c.name}`}
            data-index={i}
            role="option"
            aria-selected={i === selected}
            aria-description={c.description || undefined}
            className={`slash-menu__row ${i === selected ? "slash-menu__row--active" : ""}`}
            onMouseEnter={() => onHover(i)}
            // Keep focus in the prompt.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(c)}
          >
            {c.name}
          </div>
        ))}
        {!rows.length && <div className="slash-menu__empty">{loading ? t("slash.loading") : t("slash.noMatch")}</div>}
      </div>
      {current?.description && tipTop !== null && (
        <div className="slash-menu__tip" role="tooltip" style={{ top: tipTop }}>
          {current.description}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- client-route built-ins

export interface ChoiceOption {
  id: string;
  label: string;
  hint?: string;
  checked?: boolean;
}

export type CommandDialog =
  | { kind: "help" }
  | { kind: "connectors" }
  | { kind: "rewind"; message: MessageView }
  | { kind: "choice"; title: string; options: ChoiceOption[]; onPick: (id: string) => void };

const MODES: { id: Exclude<Mode, "unknown">; label: string }[] = [
  { id: "default", label: "Default" },
  { id: "accept_edits", label: "Accept edits" },
  { id: "plan", label: "Plan" },
  { id: "bypass", label: "Bypass permissions" },
];

function modeFromArg(arg: string): Exclude<Mode, "unknown"> | null {
  const a = arg.toLowerCase().replace(/-/g, "_");
  return MODES.find((m) => m.id === a || m.label.toLowerCase() === arg.toLowerCase())?.id ?? null;
}

export async function exportSession(app: App, store: SessionStore, format: "markdown" | "json") {
  const r = await app.client.session.export({ session_id: store.id, format });
  const name = `${(store.model.info?.title ?? "chat").slice(0, 60)}.${format === "markdown" ? "md" : "json"}`;
  const path = await app.platform.saveFile(name, { text: r.text ?? "" });
  app.toast({ title: t("row.exported", { path }), tone: "success" });
}

/**
 * Run a `client` built-in (06 §9.1 table). Returns a dialog for the composer to show, or
 * null when the command already did its work (a toast, a settings page, a new session).
 */
export async function runClientCommand(app: App, store: SessionStore | null, kind: SessionKind, name: string, args: string): Promise<CommandDialog | null> {
  const info = store?.model.info;
  const workspace = store ? (app.sessions.get(store.id)?.workspace_id ?? null) : null;
  const needSession = () => {
    app.toast({ title: t("slash.needsSession", { name }), tone: "info" });
    return null;
  };
  const noMatch = (what: string) => app.toast({ title: t("slash.noMatchArg", { what, arg: args }), tone: "danger" });

  switch (name) {
    case "help":
      return { kind: "help" };

    case "model": {
      const models = app.usableModels;
      const current = info?.model ?? app.preferredModel() ?? "";
      const set = (id: string) => {
        app.setUi({ model: id });
        if (store) void app.client.session.update({ session_id: store.id, model: id }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
        app.toast({ title: t("slash.modelSet", { name: models.find((m) => m.id === id)?.name || id }), tone: "success" });
      };
      if (args) {
        const a = args.toLowerCase();
        const m = models.find((x) => x.id.toLowerCase() === a || x.name?.toLowerCase() === a) ?? models.find((x) => x.id.toLowerCase().endsWith(`/${a}`) || x.name?.toLowerCase().includes(a));
        if (m) set(m.id);
        else noMatch("model");
        return null;
      }
      return { kind: "choice", title: t("slash.modelTitle"), options: models.map((m) => ({ id: m.id, label: m.name || m.id, hint: m.provider, checked: m.id === current })), onPick: set };
    }

    case "effort": {
      const current = info?.model ?? app.preferredModel() ?? "";
      const levels = (app.models.find((m) => m.id === current)?.effort_levels ?? []).filter((e) => !isOffEffort(e));
      if (!levels.length) {
        app.toast({ title: t("slash.noEffort"), tone: "info" });
        return null;
      }
      const effort = info?.effort ?? app.ui.effort ?? levels[0];
      const set = (level: string) => {
        app.setUi({ effort: level });
        if (store) void app.client.session.update({ session_id: store.id, effort: level }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
        app.toast({ title: t("slash.effortSet", { level }), tone: "success" });
      };
      if (args) {
        const l = levels.find((x) => x.toLowerCase() === args.toLowerCase());
        if (l) set(l);
        else noMatch("effort level");
        return null;
      }
      return { kind: "choice", title: t("slash.effortTitle"), options: levels.map((l) => ({ id: l, label: l[0].toUpperCase() + l.slice(1), checked: l === effort })), onPick: set };
    }

    case "mcp": {
      const m = /^(enable|disable)\s+(\S+)$/i.exec(args);
      if (m) {
        const enabled = m[1].toLowerCase() === "enable";
        await app.client.mcp.set_enabled({ name: m[2], enabled, session_id: store?.id ?? null });
        app.toast({ title: t("slash.mcpToggled", { server: m[2], state: enabled ? "on" : "off" }), tone: "success" });
        return null;
      }
      if (store) return { kind: "connectors" };
      app.openDeepLink("genie://settings/connectors");
      return null;
    }

    case "skills":
      app.openDeepLink("genie://settings/capabilities");
      return null;
    case "memory":
      app.openDeepLink("genie://settings/genie");
      return null;
    case "permissions":
      app.openDeepLink(kind === "code" ? "genie://settings/permissions" : "genie://settings/capabilities");
      return null;

    case "cost": {
      const usd = store?.model.usage?.cost.usd;
      if (usd === undefined) app.toast({ title: t("slash.noUsage"), tone: "info", link: "genie://settings/usage" });
      else app.toast({ title: t("slash.cost", { cost: formatCost(usd) }), tone: "info", link: "genie://settings/usage" });
      return null;
    }
    case "context": {
      const u = store?.model.usage;
      if (!u?.context_window) app.toast({ title: t("slash.noUsage"), tone: "info" });
      else app.toast({ title: t("slash.context", { used: formatTokens(u.context_tokens), window: formatTokens(u.context_window), pct: Math.round((u.context_tokens / u.context_window) * 100) }), tone: "info" });
      return null;
    }

    case "resume":
      app.openSearch();
      return null;

    case "export": {
      if (!store) return needSession();
      const f = args.toLowerCase();
      if (f === "markdown" || f === "md") await exportSession(app, store, "markdown");
      else if (f === "json") await exportSession(app, store, "json");
      else if (f) noMatch("format");
      else
        return {
          kind: "choice",
          title: t("slash.exportTitle"),
          options: [
            { id: "markdown", label: t("row.exportMd") },
            { id: "json", label: t("row.exportJson") },
          ],
          onPick: (id) => void exportSession(app, store, id as "markdown" | "json").catch((e) => app.toast({ title: app.describeError(e), tone: "danger" })),
        };
      return null;
    }

    case "clear":
      if (kind === "code" && workspace) await app.createSession({ kind: "code", workspacePath: workspace });
      else app.setUi({ activeSession: null, view: "main", activeProject: info?.project_id ?? app.ui.activeProject });
      return null;

    // ---- Code
    case "mode": {
      if (!store) return needSession();
      const set = (mode: string) => {
        void app.client.session.update({ session_id: store.id, mode: mode as Mode }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" }));
        app.toast({ title: t("slash.modeSet", { mode: MODES.find((m) => m.id === mode)?.label ?? mode }), tone: "success" });
      };
      if (args) {
        const m = modeFromArg(args);
        if (m) set(m);
        else noMatch("mode");
        return null;
      }
      return { kind: "choice", title: t("slash.modeTitle"), options: MODES.map((m) => ({ id: m.id, label: m.label, checked: m.id === info?.mode })), onPick: set };
    }
    case "plan":
      if (!store) return needSession();
      await app.client.session.update({ session_id: store.id, mode: "plan" });
      app.toast({ title: t("slash.modeSet", { mode: "Plan" }), tone: "success" });
      if (args) await store.send([{ type: "text", text: args }], "steer");
      return null;
    case "compact":
      if (!store) return needSession();
      await app.client.session.compact({ session_id: store.id, focus: args || null });
      app.toast({ title: t("slash.compacting"), tone: "info" });
      return null;
    case "rewind": {
      if (!store) return needSession();
      const last = [...store.model.messages].reverse().find((m) => m.role === "user" && !m.synthetic);
      if (!last) {
        app.toast({ title: t("slash.noRewind"), tone: "info" });
        return null;
      }
      return { kind: "rewind", message: last };
    }
    case "worktree":
      if (!workspace) {
        app.toast({ title: t("slash.noWorkspace"), tone: "info" });
        return null;
      }
      await app.createSession({ kind: "code", workspacePath: workspace, worktree: true });
      return null;
    case "agents": {
      const r = await app.client.agents.list({ cwd: workspace });
      if (!r.agents.length) {
        app.toast({ title: t("slash.noAgents"), tone: "info" });
        return null;
      }
      return {
        kind: "choice",
        title: t("slash.agentTitle"),
        options: r.agents.map((a) => ({ id: a.name, label: a.name, hint: a.description })),
        onPick: (agent) => void app.createSession({ kind: "code", workspacePath: workspace ?? undefined, agent }).catch((e) => app.toast({ title: app.describeError(e), tone: "danger" })),
      };
    }
    default:
      return null;
  }
}

/** A list of choices (models, effort levels, modes, export formats, agents). */
export function ChoiceDialog({ title, options, onPick, onClose }: { title: string; options: ChoiceOption[]; onPick: (id: string) => void; onClose: () => void }) {
  return (
    <Dialog open title={title} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      <div className="stack">
        {options.map((o) => (
          <button
            key={o.id}
            className="list-button"
            aria-pressed={o.checked}
            onClick={() => {
              onClose();
              onPick(o.id);
            }}
          >
            <strong>{o.label}</strong> {o.hint && <span className="muted">{o.hint}</span>} {o.checked && <span aria-label="Selected">✓</span>}
          </button>
        ))}
      </div>
    </Dialog>
  );
}

/** `/help`: every command with its description, then the keys. */
export function HelpDialog({ commands, onClose }: { commands: CommandInfo[]; onClose: () => void }) {
  return (
    <Dialog open title={t("slash.help")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      <dl className="slash-help">
        {commands.map((c) => (
          <div key={c.name} className="slash-help__row">
            <dt>
              /{c.name}
              {c.args_hint ? <span className="muted"> {c.args_hint}</span> : null}
            </dt>
            <dd className="muted">{c.description}</dd>
          </div>
        ))}
      </dl>
      <p className="muted">{t("slash.helpKeys")}</p>
    </Dialog>
  );
}

/** "+" → Skills & commands: every non-built-in command; picking one inserts its `/token`. */
export function CommandPicker({ commands, onPick, onClose }: { commands: CommandInfo[] | null; onPick: (c: CommandInfo) => void; onClose: () => void }) {
  const rows = (commands ?? []).filter((c) => c.source !== "builtin");
  return (
    <Dialog open title={t("composer.skills")} onClose={onClose} footer={<Button onClick={onClose}>{t("common.close")}</Button>}>
      {!commands ? (
        t("common.loading")
      ) : (
        <div className="stack">
          {rows.map((c) => (
            <button key={c.name} className="list-button" onClick={() => onPick(c)}>
              <strong>/{c.name}</strong> <span className="muted">{c.description}</span>
            </button>
          ))}
          {rows.length === 0 && <p className="muted">{t("slash.none")}</p>}
        </div>
      )}
    </Dialog>
  );
}
