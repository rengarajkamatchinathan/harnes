// A column splitter (sidebar edge, right panel edge), drag-to-resize the way desktop apps do it:
// the width follows the pointer live through a CSS variable on `.shell` (no React render, no
// state.json write per move) and is committed once on release. Limits are the room actually
// available, so the handle never goes "dead" past a width the layout would clamp anyway.
// Double-click restores the default; arrows step 16 px (Shift: 64); Escape cancels a drag.
import { useRef } from "react";

export interface ResizeHandleProps {
  /** Which edge of its column the handle sits on: "right" (sidebar) grows when dragged right. */
  edge: "left" | "right";
  /** The width as shown now. */
  value: number;
  min: number;
  max: number;
  defaultValue: number;
  /** The CSS variable on `.shell` that sizes the column. */
  cssVar: string;
  label: string;
  className: string;
  onCommit: (width: number) => void;
}

const shellEl = () => document.querySelector<HTMLElement>(".shell");

export function ResizeHandle({ edge, value, min, max, defaultValue, cssVar, label, className, onCommit }: ResizeHandleProps) {
  const drag = useRef<{ x: number; start: number; last: number; frame: number } | null>(null);
  const hi = Math.max(min, max);
  const clamp = (w: number) => Math.round(Math.max(min, Math.min(hi, w)));
  const dir = edge === "right" ? 1 : -1;

  const paint = (w: number) => shellEl()?.style.setProperty(cssVar, `${w}px`);
  const finish = (el: HTMLElement, commit: boolean) => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    cancelAnimationFrame(d.frame);
    el.removeAttribute("data-dragging");
    shellEl()?.classList.remove("shell--resizing");
    const w = commit ? d.last : d.start;
    // React only rewrites the variable when its value changes, so put the final one back ourselves.
    paint(w);
    if (w !== d.start) onCommit(w);
  };
  const step = (w: number) => {
    const next = clamp(w);
    if (next !== value) onCommit(next);
  };

  return (
    <div
      className={`resize-handle ${className}`}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuemin={min}
      aria-valuemax={hi}
      aria-valuenow={value}
      tabIndex={0}
      onKeyDown={(e) => {
        if (drag.current) {
          if (e.key === "Escape") finish(e.currentTarget, false);
          return;
        }
        const by = e.shiftKey ? 64 : 16;
        if (e.key === "ArrowLeft") step(value - by * dir);
        else if (e.key === "ArrowRight") step(value + by * dir);
        else if (e.key === "Home") step(min);
        else if (e.key === "End") step(hi);
        else return;
        e.preventDefault();
      }}
      onDoubleClick={() => step(defaultValue)}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.focus({ preventScroll: true });
        e.currentTarget.setPointerCapture(e.pointerId);
        e.currentTarget.setAttribute("data-dragging", "");
        shellEl()?.classList.add("shell--resizing");
        drag.current = { x: e.clientX, start: value, last: value, frame: 0 };
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        d.last = clamp(d.start + (e.clientX - d.x) * dir);
        cancelAnimationFrame(d.frame);
        d.frame = requestAnimationFrame(() => paint(d.last));
      }}
      onPointerUp={(e) => finish(e.currentTarget, true)}
      onPointerCancel={(e) => finish(e.currentTarget, false)}
      onLostPointerCapture={(e) => finish(e.currentTarget, true)}
    />
  );
}
