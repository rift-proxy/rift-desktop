import {
  createContext,
  useContext,
  useEffect,
  useState,
  useCallback,
  useRef,
  type ReactNode,
} from "react";
import type { Action, BackendClient, Snapshot } from "./types";
import { t } from "../i18n";
const Context = createContext<BackendClient | null>(null);
export function BackendProvider({
  client,
  children,
}: {
  client: BackendClient;
  children: ReactNode;
}) {
  return <Context.Provider value={client}>{children}</Context.Provider>;
}
export function useBackend() {
  const api = useContext(Context);
  // i18n-ignore: a message for the developer who forgot the provider, never shown to a person
  if (!api) throw new Error("BackendProvider missing");
  return api;
}
export function useAppState() {
  const api = useBackend();
  const [state, setState] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const events = useRef(0);
  const readySent = useRef(false);
  useEffect(() => {
    let active = true;
    let dispose: (() => void) | undefined;
    let received = false;
    void api
      .subscribe((s) => {
        events.current += 1;
        received = true;
        if (active) setState(s);
      })
      .then((fn) => {
        if (active) dispose = fn;
        else fn();
        return api.snapshot();
      })
      .then((s) => {
        if (active && !received) setState(s);
      })
      .catch(() => {
        if (active) setError(t("backend.unreachable"));
      });
    return () => {
      active = false;
      dispose?.();
    };
  }, [api]);
  useEffect(() => {
    if (!state || readySent.current) return;
    readySent.current = true;
    // After the browser painted the first state: the screen is usable, not just rendered.
    const painted =
      globalThis.requestAnimationFrame ?? ((f) => setTimeout(f, 0));
    painted(() => setTimeout(() => void api.ready?.().catch(() => {}), 0));
  }, [state, api]);
  const execute = useCallback(
    async (action: Action) => {
      setBusy(true);
      setError(null);
      const before = events.current;
      try {
        const s = await api.execute(action);
        // IPC responses may arrive after a newer state event from a running cycle.
        if (events.current === before) setState(s);
        return true;
      } catch (e) {
        setError(typeof e === "string" ? e : t("backend.action_failed"));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [api],
  );
  return { state, error, busy, execute };
}
