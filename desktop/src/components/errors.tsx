import { Component, type ErrorInfo, type ReactNode } from "react";
import { AlertCircle, Info, TriangleAlert, X } from "lucide-react";
import type { ErrorToast } from "../backend/errors";
import { EmptyState, IconButton } from "./ui";
import { t } from "../i18n";

/** Errors the host reported, bottom right: they leave on their own and need no action. */
export function ErrorToasts({
  toasts,
  onDismiss,
}: {
  toasts: ErrorToast[];
  onDismiss: (id: number) => void;
}) {
  if (toasts.length === 0) return null;
  return (
    <div className="toasts">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`toast ${toast.tone}`}
          role={toast.tone === "info" ? "status" : "alert"}
        >
          {toast.tone === "info" ? (
            <Info size={17} />
          ) : (
            <AlertCircle size={17} />
          )}
          <span>{toast.text}</span>
          <IconButton
            label={
              toast.tone === "info"
                ? t("toast.hide_notification")
                : t("toast.hide_error")
            }
            onClick={() => onDismiss(toast.id)}
          >
            <X size={16} />
          </IconButton>
        </div>
      ))}
    </div>
  );
}

/** Arbitrary error values are handed only to Rust, which redacts and bounds local storage. */
export function describe(value: unknown): string {
  try {
    if (value instanceof Error)
      return typeof value.stack === "string"
        ? value.stack
        : `${value.name}: ${value.message}`;
    if (typeof value === "string") return value;
    return JSON.stringify(value) ?? String(value);
  } catch {
    return t("ui_error.undescribable");
  }
}

type LogError = (message: string, detail?: string) => unknown;

function logSafely(logError: LogError, detail: string) {
  try {
    void Promise.resolve(logError(t("ui_error.message"), detail)).catch(
      () => {},
    );
  } catch {
    // A local error logger must never become a second error source.
  }
}

/** Uncaught errors and rejected promises stay in the local Rust journal. */
export function installErrorHandlers(logError: LogError) {
  const onError = (event: ErrorEvent) =>
    logSafely(
      logError,
      `${describe(event.error ?? event.message)}\n${event.filename}:${event.lineno}:${event.colno}`,
    );
  const onRejection = (event: PromiseRejectionEvent) =>
    logSafely(logError, describe(event.reason));
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onRejection);
  return () => {
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onRejection);
  };
}

export class ErrorBoundary extends Component<
  { logError: LogError; children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  componentDidCatch(error: unknown, info: ErrorInfo) {
    logSafely(
      this.props.logError,
      `${describe(error)}\n${info.componentStack ?? ""}`,
    );
  }
  render() {
    // The Rust core keeps running: only this window needs a restart.
    return this.state.failed ? (
      <div className="crash-screen" role="alert">
        <EmptyState
          icon={<TriangleAlert size={36} />}
          title={t("crash.title")}
          description={t("crash.description")}
        />
      </div>
    ) : (
      this.props.children
    );
  }
}
