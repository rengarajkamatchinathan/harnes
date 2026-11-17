import { createContext, useContext, useEffect, useState } from "react";
import type { App } from "../state/app";
import { useEmitter } from "../state/emitter";
import type { SessionStore } from "../state/session";

export const AppContext = createContext<App | null>(null);

/** The app controller; re-renders the caller when app-level state changes. */
export function useApp(): App {
  const app = useContext(AppContext);
  if (!app) throw new Error("AppContext missing");
  useEmitter(app);
  return app;
}

/** The app controller without subscribing (for callbacks in leaf components). */
export function useAppStatic(): App {
  const app = useContext(AppContext);
  if (!app) throw new Error("AppContext missing");
  return app;
}

export function useSession(id: string | null): SessionStore | null {
  const app = useAppStatic();
  const store = id ? app.session(id) : null;
  useEmitter(store);
  return store;
}

/** Run an async loader; re-run when `deps` change. */
export function useAsync<T>(load: () => Promise<T>, deps: unknown[]): { data: T | null; error: unknown; loading: boolean; reload: () => void } {
  const [state, setState] = useState<{ data: T | null; error: unknown; loading: boolean }>({ data: null, error: null, loading: true });
  const [n, setN] = useState(0);
  useEffect(() => {
    let live = true;
    setState((s) => ({ ...s, loading: true }));
    load().then(
      (data) => live && setState({ data, error: null, loading: false }),
      (error) => live && setState({ data: null, error, loading: false }),
    );
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, n]);
  return { ...state, reload: () => setN((x) => x + 1) };
}
