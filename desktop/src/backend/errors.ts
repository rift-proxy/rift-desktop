import { useCallback, useEffect, useRef, useState } from "react";
import { useBackend } from "./provider";

/** How long an error toast stays. It never needs closing. */
export const ERROR_TOAST_MS = 10_000;
/** The same text is not shown again this soon: a failing background retry must not blink. */
export const ERROR_QUIET_MS = 300_000;
/** More toasts at once push the oldest out. */
export const ERROR_TOAST_LIMIT = 3;
const SHOWN_CHARS = 400;

export interface ErrorToast {
  id: number;
  text: string;
  /** `info`: no failure, a notice (a fallback that took place). */
  tone: "error" | "info";
}

/** What a toast shows of an error text: the journal keeps the whole of it. */
export function shownText(message: string): string {
  const text = message.trim();
  return text.length > SHOWN_CHARS
    ? `${text.slice(0, SHOWN_CHARS).trimEnd()}…`
    : text;
}

/** Errors the host reports, as toasts: the replacement of the former error window. Notices of
 * the host (no failure) take the same place, in a calmer tone. */
export function useErrorToasts(bannerText: string | null | undefined) {
  const api = useBackend();
  const [toasts, setToasts] = useState<ErrorToast[]>([]);
  const sequence = useRef(0);
  const lastShown = useRef(new Map<string, number>());
  const timers = useRef(new Map<number, ReturnType<typeof setTimeout>>());
  const dismiss = useCallback((id: number) => {
    clearTimeout(timers.current.get(id));
    timers.current.delete(id);
    setToasts((list) => list.filter((toast) => toast.id !== id));
  }, []);
  useEffect(() => {
    if (!api.onError && !api.onNotice) return;
    let active = true;
    const disposers: (() => void)[] = [];
    const pending = timers.current;
    const banner = bannerText ? shownText(bannerText) : null;
    const show = (message: string, tone: ErrorToast["tone"]) => {
      if (!active || !message.trim()) return;
      const text = shownText(message);
      // Do not create toast if banner already shows this text.
      if (banner && text === banner) return;
      const now = Date.now();
      for (const [known, at] of lastShown.current)
        if (now - at >= ERROR_QUIET_MS) lastShown.current.delete(known);
      // Dedup on original message, not truncated text.
      if (lastShown.current.has(message)) return;
      lastShown.current.set(message, now);
      const id = ++sequence.current;
      setToasts((list) =>
        [...list, { id, text, tone }].slice(-ERROR_TOAST_LIMIT),
      );
      pending.set(
        id,
        setTimeout(() => dismiss(id), ERROR_TOAST_MS),
      );
    };
    const keep = (registered: Promise<() => void> | undefined) =>
      registered
        ?.then((fn) => {
          if (active) disposers.push(fn);
          else fn();
        })
        .catch(() => {
          // Toasts are a convenience: the journal and the banner still show the error.
        });
    void keep(api.onError?.((message) => show(message, "error")));
    void keep(api.onNotice?.((message) => show(message, "info")));
    return () => {
      active = false;
      for (const dispose of disposers) dispose();
      for (const timer of pending.values()) clearTimeout(timer);
      pending.clear();
    };
  }, [api, dismiss, bannerText]);
  return { toasts, dismiss };
}
