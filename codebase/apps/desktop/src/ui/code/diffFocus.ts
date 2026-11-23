// Which file the Changes panel should open and scroll to ("click a file in the commit
// checklist"). A tiny in-memory signal, not persisted UI state.
import { useSyncExternalStore } from "react";

let focus: { path: string; n: number } | null = null;
const listeners = new Set<() => void>();

export function focusDiff(path: string) {
  focus = { path, n: (focus?.n ?? 0) + 1 };
  for (const l of listeners) l();
}

export function useDiffFocus() {
  return useSyncExternalStore(
    (l) => (listeners.add(l), () => void listeners.delete(l)),
    () => focus,
  );
}
