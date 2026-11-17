import { useSyncExternalStore } from "react";

const raf: (f: () => void) => void =
  typeof requestAnimationFrame === "function" ? (f) => requestAnimationFrame(() => f()) : (f) => setTimeout(f, 16);

/**
 * Change notification with one React commit per animation frame (05 §7): stores mutate
 * freely as events arrive and call `changed()`; subscribers run once on the next frame.
 */
export class Emitter {
  version = 0;
  private listeners = new Set<() => void>();
  private scheduled = false;

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  getVersion = () => this.version;

  /** Coalesced to the next animation frame. */
  changed() {
    this.version++;
    if (this.scheduled) return;
    this.scheduled = true;
    raf(() => {
      this.scheduled = false;
      this.listeners.forEach((f) => f());
    });
  }

  /** Immediately (user actions where a frame of delay would be visible). */
  changedNow() {
    this.version++;
    this.listeners.forEach((f) => f());
  }
}

/** Re-render when `e` changes; returns its version. */
export function useEmitter(e: Emitter | null | undefined): number {
  return useSyncExternalStore(e ? e.subscribe : noopSubscribe, e ? e.getVersion : zero, e ? e.getVersion : zero);
}

const noopSubscribe = () => () => {};
const zero = () => 0;
