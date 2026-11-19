// ⌘K command palette: one box to search chats (titles + content, FTS D24) and run commands.
// Groups: Quick actions, Navigation, Settings, Chats. Typing filters commands by label and
// runs a content search over chats; ↑/↓ move across all groups, Enter runs, Esc closes.
import { useEffect, useMemo, useRef, useState } from "react";
import type { SessionSummary } from "@genie/protocol";
import { Icon, IconButton, type IconName } from "@genie/ui";
import { t } from "../i18n";
import { useApp } from "./context";
import { SECTION_ICON } from "./settings/SettingsPage";
import { SECTIONS } from "./settings/schema";

const isMac = () => typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);
export const modKey = () => (isMac() ? "⌘" : "Ctrl ");

interface Command {
  id: string;
  group: "actions" | "nav" | "settings" | "chats";
  label: string;
  icon: IconName;
  /** Extra words that should match (e.g. "preferences" for Settings). */
  keywords?: string;
  hint?: string;
  shortcut?: string;
  run: () => void;
}

const GROUP_LABEL: Record<Command["group"], Parameters<typeof t>[0]> = {
  actions: "palette.actions",
  nav: "palette.navigation",
  settings: "palette.settings",
  chats: "palette.chats",
};

export function CommandPalette({ onClose, onNewProject, onNewCode }: { onClose: () => void; onNewProject: () => void; onNewCode: () => void }) {
  const app = useApp();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<string[] | null>(null);
  const [sel, setSel] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const q = query.trim().toLowerCase();

  // Content search over chats (debounced); falls back to a title match if the engine can't.
  useEffect(() => {
    if (!q) return setHits(null);
    const h = setTimeout(() => {
      app.client.session
        .search({ query, scope: "all" })
        .then((r) => setHits([...new Set(r.hits.map((x) => x.session_id))]))
        .catch(() => setHits([...app.sessions.values()].filter((s) => (s.title ?? "").toLowerCase().includes(q)).map((s) => s.session_id)));
    }, 150);
    return () => clearTimeout(h);
  }, [query, q, app]);

  const close = (then?: () => void) => () => {
    onClose();
    then?.();
  };
  const dark = typeof document !== "undefined" && document.documentElement.dataset.theme === "dark";

  const commands = useMemo<Command[]>(() => {
    const go = (patch: Parameters<typeof app.setUi>[0]) => () => app.setUi(patch);
    const list: Command[] = [
      { id: "new-chat", group: "actions", label: t("palette.newChat"), icon: "plus", shortcut: `${modKey()}N`, run: go({ mode: "chat", view: "main", activeSession: null, activeProject: null }) },
      { id: "new-code", group: "actions", label: t("palette.newCode"), icon: "code", run: () => (app.setUi({ mode: "code", view: "main", activeSession: null, activeProject: null }), onNewCode()) },
      { id: "new-project", group: "actions", label: t("sidebar.newProject"), icon: "projects", run: () => (app.setUi({ mode: "chat" }), onNewProject()) },
      {
        id: "theme",
        group: "actions",
        label: dark ? t("palette.lightTheme") : t("palette.darkTheme"),
        icon: "palette",
        keywords: "theme appearance dark light mode",
        run: () => app.setUi({ appearance: { ...app.ui.appearance, theme: dark ? "light" : "dark" } }),
      },
      { id: "toggle-sidebar", group: "actions", label: t("sidebar.toggle"), icon: "sidebar", shortcut: `${modKey()}.`, run: () => app.setUi({ sidebarCollapsed: !app.ui.sidebarCollapsed }) },
      { id: "home", group: "nav", label: t("palette.home"), icon: "chat", keywords: "start new", run: go({ view: "main", activeSession: null, activeProject: null }) },
      { id: "projects", group: "nav", label: t("sidebar.projects"), icon: "projects", run: go({ mode: "chat", view: "project", activeProject: null, activeSession: null }) },
      { id: "mode-chat", group: "nav", label: t("palette.chatMode"), icon: "modeChat", run: go({ mode: "chat", view: "main", activeSession: null, activeProject: null }) },
      { id: "mode-code", group: "nav", label: t("palette.codeMode"), icon: "modeCode", run: go({ mode: "code", view: "main", activeSession: null, activeProject: null }) },
      { id: "settings", group: "nav", label: t("sidebar.settings"), icon: "settings", keywords: "preferences customize", shortcut: `${modKey()},`, run: go({ view: "settings", settingsSection: "general", settingsItem: null }) },
      ...app.projects.map<Command>((p) => ({ id: `project:${p.project_id}`, group: "nav", label: p.name, icon: "folder", hint: t("palette.project"), run: go({ mode: "chat", view: "project", activeProject: p.project_id }) })),
      ...SECTIONS.map<Command>((s) => {
        const label = t(`settings.section.${s}` as Parameters<typeof t>[0]);
        return { id: `settings:${s}`, group: "settings", label, icon: SECTION_ICON[s], keywords: "settings", run: () => app.openDeepLink(`genie://settings/${s}`) };
      }),
    ];
    // Without a query, keep the palette short: settings sections only appear when searched.
    return q ? list.filter((c) => `${c.label} ${c.keywords ?? ""} ${c.hint ?? ""}`.toLowerCase().includes(q)) : list.filter((c) => c.group !== "settings" && !c.id.startsWith("project:"));
  }, [app, q, dark, onNewCode, onNewProject]);

  const chats = useMemo<Command[]>(() => {
    const ids = hits ?? [...app.sessions.values()].filter((s) => !s.archived).sort((a, b) => b.updated_at.localeCompare(a.updated_at)).map((s) => s.session_id);
    return ids
      .map((id) => app.sessions.get(id))
      .filter((s): s is SessionSummary => !!s)
      .slice(0, q ? 20 : 6)
      .map((s) => ({
        id: `chat:${s.session_id}`,
        group: "chats" as const,
        label: s.title || t("sidebar.untitled"),
        icon: (s.kind === "code" ? "code" : "chat") as IconName,
        hint: new Date(s.updated_at).toLocaleDateString(),
        run: () => app.selectSession(s.session_id),
      }));
  }, [hits, app, q]);

  const all = [...commands, ...chats];
  const active = Math.min(sel, Math.max(0, all.length - 1));
  const run = (c: Command | undefined) => c && close(c.run)();

  // Keep the highlighted row in view while arrowing through a long list.
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  let index = -1;
  const groups = (["actions", "nav", "settings", "chats"] as const).map((g) => ({ g, items: all.filter((c) => c.group === g) })).filter((x) => x.items.length);

  return (
    <div className="g-dialog__backdrop palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-modal="true" aria-label={t("palette.title")}>
        <div className="palette__input">
          <Icon name="search" />
          <input
            autoFocus
            value={query}
            placeholder={t("palette.placeholder")}
            aria-label={t("palette.title")}
            aria-controls="palette-list"
            aria-activedescendant={all[active] ? `palette-${all[active].id}` : undefined}
            onChange={(e) => (setQuery(e.target.value), setSel(0))}
            onKeyDown={(e) => {
              if (e.key === "Escape") onClose();
              else if (e.key === "ArrowDown") (e.preventDefault(), setSel((all.length ? (active + 1) % all.length : 0)));
              else if (e.key === "ArrowUp") (e.preventDefault(), setSel(all.length ? (active - 1 + all.length) % all.length : 0));
              else if (e.key === "Enter") (e.preventDefault(), run(all[active]));
            }}
          />
          <IconButton icon="x" size="sm" label={t("common.close")} onClick={onClose} />
        </div>
        <div className="palette__list" id="palette-list" role="listbox" ref={listRef}>
          {groups.map(({ g, items }) => (
            <div key={g} className="palette__section" role="group" aria-label={t(GROUP_LABEL[g])}>
              <div className="palette__group">{t(GROUP_LABEL[g])}</div>
              {items.map((c) => {
                index += 1;
                const i = index;
                return (
                  <button
                    key={c.id}
                    id={`palette-${c.id}`}
                    data-index={i}
                    role="option"
                    aria-selected={i === active}
                    className={`palette__item ${i === active ? "is-active" : ""}`}
                    onMouseMove={() => i !== active && setSel(i)}
                    onClick={() => run(c)}
                  >
                    <Icon name={c.icon} />
                    <span className="palette__label">{c.label}</span>
                    {c.hint && <span className="muted palette__when">{c.hint}</span>}
                    {c.shortcut && <kbd className="palette__kbd">{c.shortcut}</kbd>}
                  </button>
                );
              })}
            </div>
          ))}
          {all.length === 0 && <div className="muted palette__empty">{q ? t("palette.noResults", { q: query.trim() }) : t("sidebar.empty")}</div>}
        </div>
        <div className="palette__footer muted" aria-hidden>
          <span><kbd className="palette__kbd">↑</kbd><kbd className="palette__kbd">↓</kbd> {t("palette.navigate")}</span>
          <span><kbd className="palette__kbd">↵</kbd> {t("palette.open")}</span>
          <span><kbd className="palette__kbd">Esc</kbd> {t("palette.close")}</span>
        </div>
      </div>
    </div>
  );
}
