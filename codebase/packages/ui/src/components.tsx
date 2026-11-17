// Core components (05 §6, §11). Plain React on the tokens; keyboard and ARIA handled here
// so app code doesn't have to.
import {
  useEffect,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
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

export function IconButton({ icon, label, variant = "ghost", size = "md", className, type = "button", ...rest }: IconButtonProps) {
  return (
    <button
      type={type}
      aria-label={label}
      title={label}
      className={cx("g-btn", "g-btn--icon", `g-btn--${variant}`, size === "sm" && "g-btn--sm", className)}
      {...rest}
    >
      <Icon name={icon} />
    </button>
  );
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

export interface SelectProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, "onChange"> {
  options: SelectOption[];
  onChange?: (value: string) => void;
}

/** Native select (accessible, keyboard-friendly); options with `group` become optgroups. */
export function Select({ options, onChange, className, ...rest }: SelectProps) {
  const groups = new Map<string, SelectOption[]>();
  for (const o of options) {
    const g = o.group ?? "";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g)!.push(o);
  }
  const render = (o: SelectOption) => (
    <option key={o.value} value={o.value} disabled={o.disabled}>
      {o.label}
    </option>
  );
  return (
    <select className={cx("g-select", className)} onChange={(e) => onChange?.(e.target.value)} {...rest}>
      {[...groups.entries()].map(([g, opts]) =>
        g ? (
          <optgroup key={g} label={g}>
            {opts.map(render)}
          </optgroup>
        ) : (
          opts.map(render)
        ),
      )}
    </select>
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
  return (
    <div className="g-dialog__backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
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
  danger?: boolean;
  disabled?: boolean;
  title?: string;
  onSelect: () => void;
}

/** Button + popup menu; closes on outside click, Esc and selection. */
export function Menu({
  trigger,
  items,
  label,
  placement = "down",
  align = "left",
}: {
  trigger: (props: { onClick: () => void; "aria-expanded": boolean; "aria-haspopup": "menu" }) => ReactNode;
  items: MenuItem[];
  label: string;
  placement?: "down" | "up";
  align?: "left" | "right";
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    ref.current?.querySelector<HTMLElement>(".g-menu__item:not(:disabled)")?.focus();
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  const onKey = (e: ReactKeyboardEvent) => {
    if (e.key === "Escape") setOpen(false);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const list = [...(ref.current?.querySelectorAll<HTMLElement>(".g-menu__item:not(:disabled)") ?? [])];
      const i = list.indexOf(document.activeElement as HTMLElement);
      const n = e.key === "ArrowDown" ? (i + 1) % list.length : (i - 1 + list.length) % list.length;
      list[n]?.focus();
    }
  };
  return (
    <div className="g-menu" ref={ref} onKeyDown={onKey}>
      {trigger({ onClick: () => setOpen((o) => !o), "aria-expanded": open, "aria-haspopup": "menu" })}
      {open && (
        <ul className={cx("g-menu__list", placement === "up" && "g-menu__list--up", align === "right" && "g-menu__list--right")} role="menu" aria-label={label}>
          {items.map((it) => (
            <li key={it.id} role="none">
              <button
                type="button"
                role="menuitem"
                className={cx("g-menu__item", it.danger && "g-menu__item--danger")}
                disabled={it.disabled}
                title={it.title}
                onClick={() => {
                  setOpen(false);
                  it.onSelect();
                }}
              >
                {it.icon && <Icon name={it.icon} />}
                {it.label}
              </button>
            </li>
          ))}
        </ul>
      )}
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
}: {
  label: ReactNode;
  description?: ReactNode;
  error?: string | null;
  saved?: boolean;
  locked?: string | null;
  htmlFor?: string;
  children: ReactNode;
  extra?: ReactNode;
}) {
  return (
    <div className="g-field">
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
      {description && <div className="g-field__desc">{description}</div>}
      {children}
      {error && (
        <div className="g-field__error" role="alert">
          {error}
        </div>
      )}
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
