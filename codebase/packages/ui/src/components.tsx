// Core components (05 §6, §11). React on the tokens; popovers (menu, select, tooltip) use Radix
// primitives (D19) for positioning, focus and keyboard handling so app code doesn't have to.
import {
  createContext,
  Fragment,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type CSSProperties,
  type InputHTMLAttributes,
  type ReactElement,
  type ReactNode,
  type TextareaHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import * as RMenu from "@radix-ui/react-dropdown-menu";
import * as RSelect from "@radix-ui/react-select";
import * as RTooltip from "@radix-ui/react-tooltip";
import { Icon, type IconName } from "./icons";

const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  icon?: IconName;
}

export function Button({ variant = "secondary", size = "md", icon, className, children, type = "button", ...rest }: ButtonProps) {
  return (
    <button type={type} className={cx("g-btn", `g-btn--${variant}`, size === "sm" && "g-btn--sm", className)} {...rest}>
      {icon && <Icon name={icon} />}
      {children}
    </button>
  );
}

export interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  icon: IconName;
  /** Accessible name (also the tooltip). */
  label: string;
  variant?: ButtonVariant;
  size?: "sm" | "md";
}

export function IconButton({ icon, label, variant = "ghost", size = "md", className, type = "button", tooltip = true, shortcut, ...rest }: IconButtonProps & { tooltip?: boolean; shortcut?: string }) {
  const btn = (
    <button type={type} aria-label={label} className={cx("g-btn", "g-btn--icon", `g-btn--${variant}`, size === "sm" && "g-btn--sm", className)} {...rest}>
      <Icon name={icon} />
    </button>
  );
  return tooltip ? (
    <Tooltip content={label} shortcut={shortcut}>
      {btn}
    </Tooltip>
  ) : (
    btn
  );
}

/* ---------- Tooltip (Radix) ---------- */

const TooltipScope = createContext(false);

/** One shared tooltip delay group for the app (hovering from one icon to the next shows at once). */
export function UiProvider({ children }: { children: ReactNode }) {
  return (
    <RTooltip.Provider delayDuration={450} skipDelayDuration={300}>
      <TooltipScope.Provider value={true}>{children}</TooltipScope.Provider>
    </RTooltip.Provider>
  );
}

/** Hover/focus tooltip in a portal, collision-aware. `children` must be one element that takes a ref. */
export function Tooltip({ content, shortcut, side = "bottom", children }: { content: ReactNode; shortcut?: string; side?: "top" | "bottom" | "left" | "right"; children: ReactElement }) {
  const scoped = useContext(TooltipScope);
  const tip = (
    <RTooltip.Root>
      <RTooltip.Trigger asChild>{children}</RTooltip.Trigger>
      <RTooltip.Portal>
        <RTooltip.Content className="g-tooltip" side={side} sideOffset={6} collisionPadding={8}>
          {content}
          {shortcut && <span className="g-tooltip__kbd">{shortcut}</span>}
        </RTooltip.Content>
      </RTooltip.Portal>
    </RTooltip.Root>
  );
  return scoped ? tip : <RTooltip.Provider delayDuration={450}>{tip}</RTooltip.Provider>;
}

export function Input({ className, ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return <input className={cx("g-input", className)} {...rest} />;
}

export function Textarea({ className, ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className={cx("g-textarea", className)} {...rest} />;
}

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
  group?: string;
}

export interface SelectProps {
  options: SelectOption[];
  value?: string;
  onChange?: (value: string) => void;
  disabled?: boolean;
  id?: string;
  className?: string;
  style?: CSSProperties;
  placeholder?: string;
  "aria-label"?: string;
}

// Radix Select reserves "" for "no value"; map it to a sentinel so "" stays a real option.
const EMPTY = "\u0000empty";
const toRadix = (v: string) => (v === "" ? EMPTY : v);
const fromRadix = (v: string) => (v === EMPTY ? "" : v);

/** Custom dropdown (Radix Select): keyboard + typeahead, popover in a portal; `group` adds section labels. */
export function Select({ options, value, onChange, disabled, id, className, style, placeholder, "aria-label": ariaLabel }: SelectProps) {
  const groups = new Map<string, SelectOption[]>();
  for (const o of options) {
    const g = o.group ?? "";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g)!.push(o);
  }
  const current = options.find((o) => o.value === value);
  const render = (o: SelectOption) => (
    <RSelect.Item key={o.value} value={toRadix(o.value)} disabled={o.disabled} className="g-menu__item g-select__item">
      <RSelect.ItemText>{o.label}</RSelect.ItemText>
      <RSelect.ItemIndicator className="g-menu__check">
        <Icon name="check" />
      </RSelect.ItemIndicator>
    </RSelect.Item>
  );
  return (
    <RSelect.Root value={value === undefined ? undefined : toRadix(value)} onValueChange={(v) => onChange?.(fromRadix(v))} disabled={disabled}>
      <RSelect.Trigger id={id} className={cx("g-select", className)} style={style} aria-label={ariaLabel}>
        <span className="g-select__value">{current?.label ?? placeholder ?? ""}</span>
        <RSelect.Icon className="g-select__chev">
          <Icon name="chevronDown" size={14} />
        </RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content className="g-menu__list g-select__list" position="popper" sideOffset={4} collisionPadding={8}>
          <RSelect.Viewport>
            {[...groups.entries()].map(([g, opts]) =>
              g ? (
                <RSelect.Group key={g}>
                  <RSelect.Label className="g-menu__label">{g}</RSelect.Label>
                  {opts.map(render)}
                </RSelect.Group>
              ) : (
                opts.map(render)
              ),
            )}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

export interface SwitchProps {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
  id?: string;
  "aria-label"?: string;
}

export function Switch({ checked, onChange, label, disabled, id, ...aria }: SwitchProps) {
  const auto = useId();
  const sid = id ?? auto;
  return (
    <span className="g-switch">
      <button
        id={sid}
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={aria["aria-label"]}
        disabled={disabled}
        className="g-switch__track"
        onClick={() => onChange(!checked)}
      />
      {label && <label htmlFor={sid}>{label}</label>}
    </span>
  );
}

export type Tone = "neutral" | "accent" | "success" | "warning" | "danger" | "info";

export function Badge({ tone = "neutral", children, title }: { tone?: Tone; children: ReactNode; title?: string }) {
  return (
    <span className={cx("g-badge", tone !== "neutral" && `g-badge--${tone}`)} title={title}>
      {children}
    </span>
  );
}

export function Chip({ children, onRemove, removeLabel, icon, title }: { children: ReactNode; onRemove?: () => void; removeLabel?: string; icon?: IconName; title?: string }) {
  return (
    <span className="g-chip" title={title}>
      {icon && <Icon name={icon} />}
      <span className="g-chip__text">{children}</span>
      {onRemove && <IconButton icon="x" label={removeLabel ?? "Remove"} size="sm" onClick={onRemove} />}
    </span>
  );
}

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cx("g-card", className)}>{children}</div>;
}

export function Banner({
  tone = "info",
  icon,
  children,
  actions,
  role,
}: {
  tone?: "info" | "warning" | "danger" | "success" | "neutral";
  icon?: IconName;
  children: ReactNode;
  actions?: ReactNode;
  role?: "status" | "alert";
}) {
  return (
    <div className={cx("g-banner", tone !== "info" && `g-banner--${tone}`)} role={role ?? (tone === "danger" ? "alert" : "status")}>
      {icon && <Icon name={icon} />}
      <div className="g-banner__body">{children}</div>
      {actions && <div className="g-banner__actions">{actions}</div>}
    </div>
  );
}

export function Spinner({ label }: { label?: string }) {
  return <span className="g-spinner" role="progressbar" aria-label={label ?? "Loading"} />;
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Modal dialog: focus moves in, Tab is trapped, Esc closes, focus returns on close. */
export function Dialog({
  open,
  title,
  onClose,
  children,
  footer,
  width,
}: {
  open: boolean;
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useEffect(() => {
    if (!open) return;
    const prev = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? ref.current)?.focus();
    return () => prev?.focus?.();
  }, [open]);
  if (!open) return null;
  const onKey = (e: ReactKeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    } else if (e.key === "Tab" && ref.current) {
      const items = [...ref.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };
  // Portalled to <body> so a dialog opened from inside a clipped, masked or transformed
  // container (e.g. a sidebar row's ⋯ menu) still covers the whole window. React events still
  // bubble to the owner through a portal, so stop them at the backdrop: a right-click or click
  // inside the dialog must not trigger the row's own handlers.
  const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();
  const node = (
    <div
      className="g-dialog__backdrop"
      onMouseDown={(e) => {
        stop(e);
        if (e.target === e.currentTarget) onClose();
      }}
      onClick={stop}
      onContextMenu={stop}
      onPointerDown={stop}
    >
      <div
        ref={ref}
        className="g-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={onKey}
        style={width ? { width: `min(${width}px, calc(100vw - 32px))` } : undefined}
      >
        <h2 id={titleId} className="g-dialog__title">
          {title}
        </h2>
        {children}
        {footer && <div className="g-dialog__footer">{footer}</div>}
      </div>
    </div>
  );
  // No DOM (server render in tests): render in place.
  return typeof document === "undefined" ? node : createPortal(node, document.body);
}

export interface TabItem {
  id: string;
  label: ReactNode;
  disabled?: boolean;
}

/** Tab strip with roving focus (arrow keys, Home/End). */
export function Tabs({ tabs, value, onChange, label }: { tabs: TabItem[]; value: string; onChange: (id: string) => void; label: string }) {
  const onKey = (e: ReactKeyboardEvent, i: number) => {
    const enabled = tabs.filter((t) => !t.disabled);
    const pos = enabled.findIndex((t) => t.id === tabs[i].id);
    let next: TabItem | undefined;
    if (e.key === "ArrowRight") next = enabled[(pos + 1) % enabled.length];
    else if (e.key === "ArrowLeft") next = enabled[(pos - 1 + enabled.length) % enabled.length];
    else if (e.key === "Home") next = enabled[0];
    else if (e.key === "End") next = enabled[enabled.length - 1];
    if (next) {
      e.preventDefault();
      onChange(next.id);
      (e.currentTarget.parentElement?.querySelector(`[data-tab="${next.id}"]`) as HTMLElement | null)?.focus();
    }
  };
  return (
    <div className="g-tabs" role="tablist" aria-label={label}>
      {tabs.map((t, i) => (
        <button
          key={t.id}
          type="button"
          role="tab"
          data-tab={t.id}
          className="g-tab"
          aria-selected={t.id === value}
          tabIndex={t.id === value ? 0 : -1}
          disabled={t.disabled}
          onClick={() => onChange(t.id)}
          onKeyDown={(e) => onKey(e, i)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export function Segmented<T extends string>({ options, value, onChange, label }: { options: { value: T; label: ReactNode }[]; value: T; onChange: (v: T) => void; label: string }) {
  return (
    <div className="g-segmented" role="group" aria-label={label}>
      {options.map((o) => (
        <button key={o.value} type="button" aria-pressed={o.value === value} onClick={() => onChange(o.value)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export interface MenuItem {
  id: string;
  label: ReactNode;
  icon?: IconName;
  /** Secondary text under or beside the label (e.g. a model's provider). */
  hint?: ReactNode;
  /** Shows a check at the right edge (the current choice). */
  checked?: boolean;
  /** Section label shown above this item when it differs from the previous item's. */
  group?: string;
  danger?: boolean;
  disabled?: boolean;
  title?: string;
  /** Keyboard shortcut shown at the right edge (display only). */
  shortcut?: string;
  /** Opens outside the app: shows an arrow at the right edge. */
  external?: boolean;
  /** Draws a separator above this item (without a group label). */
  separatorBefore?: boolean;
  /**
   * The action moves focus elsewhere (an inline editor, a dialog): don't hand focus back to the
   * trigger when the menu closes, or it would blur (and so cancel) what the action just opened.
   */
  keepFocus?: boolean;
  onSelect: () => void;
}

/**
 * Button + popup menu (Radix DropdownMenu): portalled, collision-aware, typeahead, arrow keys,
 * closes on outside click / Esc / selection. `trigger` gets props to spread on one element.
 * Pass `open`/`onOpenChange` to control it (e.g. to open it from a right-click elsewhere).
 */
export function Menu({
  trigger,
  items,
  label,
  placement = "down",
  align = "left",
  className,
  header,
  open: openProp,
  onOpenChange,
}: {
  trigger: (props: { "aria-expanded": boolean; "aria-haspopup": "menu" }) => ReactElement;
  items: MenuItem[];
  label: string;
  placement?: "down" | "up";
  align?: "left" | "right";
  /** Extra class on the popup list. */
  className?: string;
  /** Non-interactive line at the top of the list (e.g. the signed-in account). */
  header?: ReactNode;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const [own, setOwn] = useState(false);
  const open = openProp ?? own;
  const setOpen = (v: boolean) => (onOpenChange ? onOpenChange(v) : setOwn(v));
  const keepFocus = useRef(false);
  return (
    <div className="g-menu">
      <RMenu.Root open={open} onOpenChange={setOpen} modal={false}>
        <RMenu.Trigger asChild>{trigger({ "aria-expanded": open, "aria-haspopup": "menu" })}</RMenu.Trigger>
        <RMenu.Portal>
          <RMenu.Content
            onCloseAutoFocus={(e) => {
              if (keepFocus.current) e.preventDefault();
              keepFocus.current = false;
            }}
            className={cx("g-menu__list", className)}
            side={placement === "up" ? "top" : "bottom"}
            align={align === "right" ? "end" : "start"}
            sideOffset={6}
            collisionPadding={8}
            loop
            aria-label={label}
          >
            {header && <div className="g-menu__header">{header}</div>}
            {items.map((it, i) => (
              <Fragment key={it.id}>
                {it.group && it.group !== items[i - 1]?.group && (
                  <>
                    {i > 0 && <RMenu.Separator className="g-menu__sep" />}
                    <RMenu.Label className="g-menu__label">{it.group}</RMenu.Label>
                  </>
                )}
                {it.separatorBefore && i > 0 && <RMenu.Separator className="g-menu__sep" />}
                <RMenu.Item className={cx("g-menu__item", it.danger && "g-menu__item--danger")} disabled={it.disabled} title={it.title} onSelect={() => ((keepFocus.current = !!it.keepFocus), it.onSelect())}>
                  {it.icon && it.icon !== "check" && <Icon name={it.icon} />}
                  <span className="g-menu__text">
                    <span className="g-menu__label-text">{it.label}</span>
                    {it.hint && <span className="g-menu__hint">{it.hint}</span>}
                  </span>
                  {it.shortcut && <span className="g-menu__shortcut">{it.shortcut}</span>}
                  {it.external && (
                    <span className="g-menu__external" aria-hidden>
                      <Icon name="external" size={14} />
                    </span>
                  )}
                  {(it.checked || it.icon === "check") && (
                    <span className="g-menu__check" aria-label="Selected">
                      <Icon name="check" />
                    </span>
                  )}
                </RMenu.Item>
              </Fragment>
            ))}
          </RMenu.Content>
        </RMenu.Portal>
      </RMenu.Root>
    </div>
  );
}

/** Labelled settings row: description, inline "Saved"/error, admin lock (05 §4). */
export function Field({
  label,
  description,
  error,
  saved,
  locked,
  htmlFor,
  children,
  extra,
  inline,
}: {
  label: ReactNode;
  description?: ReactNode;
  error?: string | null;
  saved?: boolean;
  locked?: string | null;
  htmlFor?: string;
  children: ReactNode;
  extra?: ReactNode;
  /** Label and description on the left, the control on the right (settings rows). */
  inline?: boolean;
}) {
  const head = (
    <div className="g-field__head">
      <label className="g-field__label" htmlFor={htmlFor}>
        {label}
      </label>
      {locked && (
        <span className="g-field__lock">
          <Icon name="lock" /> {locked}
        </span>
      )}
      {saved && !error && <span className="g-field__saved" role="status">Saved</span>}
      {extra}
    </div>
  );
  const err = error && (
    <div className="g-field__error" role="alert">
      {error}
    </div>
  );
  if (inline)
    return (
      <div className="g-field g-field--inline">
        <div className="g-field__text">
          {head}
          {description && <div className="g-field__desc">{description}</div>}
        </div>
        <div className="g-field__control">{children}</div>
        {err}
      </div>
    );
  return (
    <div className="g-field">
      {head}
      {description && <div className="g-field__desc">{description}</div>}
      {children}
      {err}
    </div>
  );
}

/** Centred placeholder for a panel or list with nothing in it yet: icon, title, hint, action. */
export function EmptyState({ icon, title, hint, action }: { icon?: IconName; title: ReactNode; hint?: ReactNode; action?: ReactNode }) {
  return (
    <div className="g-empty">
      {icon && (
        <span className="g-empty__icon" aria-hidden>
          <Icon name={icon} size={20} />
        </span>
      )}
      <div className="g-empty__title">{title}</div>
      {hint && <div className="g-empty__hint">{hint}</div>}
      {action && <div className="g-empty__action">{action}</div>}
    </div>
  );
}

/** Circular meter (context ring, D32). `value` is 0..1. */
export function Ring({ value, size = 20, warn, label }: { value: number; size?: number; warn?: boolean; label: string }) {
  const r = (size - 4) / 2;
  const c = 2 * Math.PI * r;
  const v = Math.max(0, Math.min(1, value));
  return (
    <svg width={size} height={size} className={cx("g-ring", warn && "g-ring--warn")} role="img" aria-label={label}>
      <circle className="g-ring__track" cx={size / 2} cy={size / 2} r={r} fill="none" strokeWidth={3} />
      <circle
        className="g-ring__value"
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        strokeWidth={3}
        strokeDasharray={c}
        strokeDashoffset={c * (1 - v)}
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
        strokeLinecap="round"
      />
    </svg>
  );
}

export function Progress({ value, warn, label }: { value: number; warn?: boolean; label: string }) {
  const v = Math.max(0, Math.min(1, value));
  return (
    <div className={cx("g-progress", warn && "g-progress--warn")} role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(v * 100)}>
      <div style={{ width: `${v * 100}%` }} />
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="g-kbd">{children}</kbd>;
}
