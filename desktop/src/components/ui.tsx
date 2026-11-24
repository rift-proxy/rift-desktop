import { createPortal } from "react-dom";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useId,
  type ReactNode,
  type ComponentProps,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type RefObject,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";
import {
  X,
  LoaderCircle,
  Info,
  CheckCircle2,
  AlertCircle,
  Copy,
} from "lucide-react";
import { latency, connectionLabel, statusTitle } from "../format";
import riftMarkUrl from "../../src-tauri/icons/source.svg";
import { t, tFor } from "../i18n";

export function RiftMark({ className }: { className?: string }) {
  return (
    <img src={riftMarkUrl} className={className} alt="" aria-hidden="true" />
  );
}
export function Button({
  variant = "secondary",
  loading = false,
  children,
  ...props
}: ComponentProps<"button"> & {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  loading?: boolean;
}) {
  return (
    <button
      {...props}
      disabled={props.disabled || loading}
      className={`button ${variant} ${props.className ?? ""}`}
    >
      {loading && <LoaderCircle size={16} className="spin" />}
      {children}
    </button>
  );
}
export function IconButton({
  label,
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <Button {...props} variant="ghost" aria-label={label} title={label}>
      {children}
    </Button>
  );
}
export function Toggle({
  label,
  checked,
  onChange,
  disabled = false,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className="toggle-label">
      <span>{label}</span>
      <button
        type="button"
        className="toggle"
        role="switch"
        aria-label={label}
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
      >
        <span />
      </button>
    </label>
  );
}
export function Checkbox(
  props: InputHTMLAttributes<HTMLInputElement> & { label: string },
) {
  const { label, ...input } = props;
  return (
    <label className="choice">
      <input {...input} type="checkbox" />
      {label}
    </label>
  );
}
export function Radio(
  props: InputHTMLAttributes<HTMLInputElement> & { label: string },
) {
  const { label, ...input } = props;
  return (
    <label className="choice">
      <input {...input} type="radio" />
      {label}
    </label>
  );
}
export function Field({
  label,
  error,
  ...props
}: InputHTMLAttributes<HTMLInputElement> & { label: string; error?: string }) {
  const id = useId();
  return (
    <label className="field" htmlFor={id}>
      <span>{label}</span>
      <input
        {...props}
        id={id}
        aria-invalid={!!error}
        aria-describedby={error ? `${id}-error` : undefined}
      />
      {error && (
        <small id={`${id}-error`} className="error-text">
          {error}
        </small>
      )}
    </label>
  );
}
export function TextArea({
  label,
  error,
  ...props
}: TextareaHTMLAttributes<HTMLTextAreaElement> & {
  label: string;
  error?: string;
}) {
  const id = useId();
  return (
    <label className="field" htmlFor={id}>
      <span>{label}</span>
      <textarea
        {...props}
        id={id}
        aria-invalid={!!error}
        aria-describedby={error ? `${id}-error` : undefined}
      />
      {error && (
        <small id={`${id}-error`} className="error-text">
          {error}
        </small>
      )}
    </label>
  );
}
export function Select({
  label,
  children,
  ...props
}: SelectHTMLAttributes<HTMLSelectElement> & { label: string }) {
  return (
    <label className="field">
      <span>{label}</span>
      <select {...props}>{children}</select>
    </label>
  );
}
export function Segmented({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (v: string) => void;
}) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {options.map((o) => (
        <button
          key={o.value}
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
export function Badge({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "success" | "error" | "info";
}) {
  return <span className={`badge ${tone}`}>{children}</span>;
}
/** Space between a mark and its tip. */
const TIP_GAP = 6;
/**
 * A short explanation that comes up when the pointer is on the thing or the focus is: a native
 * `title` does not come up for the keyboard. It is read as the description of what it explains.
 */
export function Tip({
  text,
  children,
  testId,
}: {
  text: string;
  children: ReactNode;
  testId?: string;
}) {
  // Where the mark is, and once the bubble is measured where it fits in the window.
  const [at, setAt] = useState<{
    left: number;
    top: number;
    bottom: number;
    fit?: { left: number; top: number };
  } | null>(null);
  const bubble = useRef<HTMLDivElement>(null);
  const id = useId();
  const show = (element: HTMLElement) => {
    const box = element.getBoundingClientRect();
    setAt({ left: box.left, top: box.top, bottom: box.bottom });
  };
  // Below the mark, above it when there is no room below, and not past the right edge: a status
  // near the bottom or the right of the window keeps its tip in sight.
  useLayoutEffect(() => {
    const element = bubble.current;
    if (!at || at.fit || !element) return;
    const box = element.getBoundingClientRect();
    const below = at.bottom + TIP_GAP;
    setAt({
      ...at,
      fit: {
        left: Math.max(0, Math.min(at.left, window.innerWidth - box.width)),
        top:
          below + box.height > window.innerHeight
            ? Math.max(0, at.top - TIP_GAP - box.height)
            : below,
      },
    });
  }, [at]);
  return (
    <span
      className="tip"
      data-testid={testId}
      tabIndex={0}
      aria-describedby={at ? id : undefined}
      onMouseEnter={(event) => show(event.currentTarget)}
      onMouseLeave={() => setAt(null)}
      onFocus={(event) => show(event.currentTarget)}
      onBlur={() => setAt(null)}
      onKeyDown={(event) => {
        if (event.key === "Escape") setAt(null);
      }}
    >
      {children}
      {at &&
        createPortal(
          <div
            ref={bubble}
            role="tooltip"
            id={id}
            className="tip-bubble"
            style={{
              left: at.fit?.left ?? at.left,
              top: at.fit?.top ?? at.bottom + TIP_GAP,
            }}
          >
            {text}
          </div>,
          document.body,
        )}
    </span>
  );
}
export function StatusChip({
  status,
  label,
  checkedAt,
}: {
  status: string;
  label?: string;
  /** When the status was set (Unix seconds), `null` for never: the chip has a tip. Without it, none. */
  checkedAt?: number | null;
}) {
  const good = status === "online" || status === "connected";
  const bad = ["failed", "unreachable", "error"].includes(status);
  const chip = (
    <Badge tone={good ? "success" : bad ? "error" : "neutral"}>
      {good ? (
        <CheckCircle2 size={12} />
      ) : bad ? (
        <AlertCircle size={12} />
      ) : (
        <span className="dot" />
      )}
      {label ?? tFor("server_status", status) ?? t("server_status.unknown")}
    </Badge>
  );
  return checkedAt === undefined ? (
    chip
  ) : (
    <Tip text={statusTitle(checkedAt)} testId="status-chip">
      {chip}
    </Tip>
  );
}
export function Alert({
  children,
  tone = "error",
}: {
  children: ReactNode;
  tone?: "error" | "info";
}) {
  return (
    <div
      className={`alert ${tone}`}
      role={tone === "error" ? "alert" : "status"}
    >
      {tone === "error" ? <AlertCircle size={17} /> : <Info size={17} />}
      <span>{children}</span>
    </div>
  );
}
export function Tooltip({
  text,
  placement = "up",
}: {
  text: string;
  placement?: "up" | "down";
}) {
  return (
    <span
      className={`tooltip ${placement === "down" ? "tooltip-down" : ""}`}
      role="img"
      tabIndex={0}
      aria-label={text}
    >
      <Info size={15} />
      <span role="tooltip">{text}</span>
    </span>
  );
}
export function Notification({
  children,
  onClose,
  action,
}: {
  children: ReactNode;
  onClose: () => void;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="notification">
      <span role="status">{children}</span>
      {action && (
        <Button variant="primary" onClick={action.onClick}>
          {action.label}
        </Button>
      )}
      <IconButton label={t("common.close_notification")} onClick={onClose}>
        <X size={16} />
      </IconButton>
    </div>
  );
}
export function Dialog({
  title,
  children,
  onClose,
  closeDisabled = false,
  hideClose = false,
  returnFocus,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  /** No close icon: Escape still closes, and the dialog offers its own way out. */
  hideClose?: boolean;
  /** The dialog cannot be dismissed (close icon, Escape) while an operation runs. */
  closeDisabled?: boolean;
  /** Where focus goes on close when the opening control was disabled before the dialog
   * appeared (the browser then drops focus to the page). Must be a stable ref. */
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const id = useId();
  useEffect(() => {
    const dialog = ref.current;
    // React removes the element before this cleanup runs, so the browser cannot restore
    // focus itself: return it to the control that opened the dialog.
    const opener =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const target = returnFocus?.current ?? opener;
    dialog?.showModal();
    // The browser focuses the first control, which is the close icon. A form dialog starts in
    // its first field; a confirmation starts on the safe answer, the first action (Cancel).
    (
      dialog?.querySelector<HTMLElement>(
        "input:not([type=hidden]), select, textarea",
      ) ?? dialog?.querySelector<HTMLElement>(".dialog-actions button:enabled")
    )?.focus();
    return () => {
      dialog?.close();
      target?.focus();
    };
  }, [returnFocus]);
  useEffect(() => {
    // Chromium 153 (measured) stops honouring a prevented `cancel` after repeated Escape
    // presses without a click in between; WebView2 is expected to do the same but is not
    // verified. A busy dialog therefore also tells the browser that close requests are not
    // allowed.
    ref.current?.setAttribute(
      "closedby",
      closeDisabled ? "none" : "closerequest",
    );
  }, [closeDisabled]);
  return (
    <dialog
      ref={ref}
      aria-labelledby={id}
      onCancel={(e) => {
        // Escape asks to close the dialog; while it is busy the answer is no, exactly like
        // the disabled close icon and Cancel button.
        e.preventDefault();
        if (!closeDisabled) onClose();
      }}
      onClose={(e) => {
        // A browser without `closedby` support may still close a busy dialog by itself.
        // The operation is running, so put it back; an unmounted dialog stays closed.
        const dialog = e.currentTarget;
        if (closeDisabled && dialog.isConnected && !dialog.open) {
          dialog.showModal();
        }
      }}
    >
      <div className="dialog-heading">
        <h2 id={id}>{title}</h2>
        {!hideClose && (
          <IconButton
            label={t("dialog.close")}
            onClick={onClose}
            disabled={closeDisabled}
          >
            <X size={19} />
          </IconButton>
        )}
      </div>
      {children}
    </dialog>
  );
}
export function ConfirmationDialog({
  title,
  message,
  onConfirm,
  onClose,
  confirmLabel = t("common.delete"),
  confirmVariant = "danger",
  loading = false,
  returnFocus,
}: {
  title: string;
  message: ReactNode;
  onConfirm: () => void;
  onClose: () => void;
  confirmLabel?: string;
  confirmVariant?: "primary" | "danger";
  /** While true the dialog cannot be closed by any means (Escape, close icon, Cancel). */
  loading?: boolean;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  return (
    <Dialog
      title={title}
      onClose={onClose}
      closeDisabled={loading}
      returnFocus={returnFocus}
    >
      <div className="dialog-message">{message}</div>
      <div className="dialog-actions">
        <Button type="button" onClick={onClose} disabled={loading}>
          {t("common.cancel")}
        </Button>
        <Button
          type="button"
          variant={confirmVariant}
          onClick={onConfirm}
          loading={loading}
        >
          {confirmLabel}
        </Button>
      </div>
    </Dialog>
  );
}
export function Toolbar({ children }: { children: ReactNode }) {
  return <div className="toolbar">{children}</div>;
}
export function EmptyState({
  title,
  description,
  icon,
  level = 3,
  children,
}: {
  title: string;
  description: string;
  /** Only where the icon says something the title does not. */
  icon?: ReactNode;
  /** The heading level; 2 on a page that has no heading between its title and this one. */
  level?: 2 | 3;
  children?: ReactNode;
}) {
  const Heading = level === 2 ? "h2" : "h3";
  return (
    <div className="empty">
      {icon}
      <Heading>{title}</Heading>
      <p>{description}</p>
      {children}
    </div>
  );
}
export function LoadingState({
  label = t("common.loading"),
}: {
  label?: string;
}) {
  return (
    <div className="empty" role="status">
      <LoaderCircle size={30} className="spin" />
      <p>{label}</p>
    </div>
  );
}
export function ErrorState({ message }: { message: string }) {
  return <EmptyState title={t("common.load_failed")} description={message} />;
}
export function LatencyDisplay({
  endpoint,
  proxy,
}: {
  endpoint: number | null;
  proxy: number | null;
}) {
  return (
    <div className="latencies">
      <div>
        <span>{t("latency.endpoint")}</span>
        <strong>{latency(endpoint)}</strong>
        <small>{t("latency.endpoint_hint")}</small>
      </div>
      <div>
        <span>{t("latency.proxy")}</span>
        <strong>{latency(proxy)}</strong>
        <small>{t("latency.proxy_hint")}</small>
      </div>
    </div>
  );
}
export function ProxyAddress({ port }: { port: number }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <div className="proxy-address">
      <span>{t("proxy_address.protocols")}</span>
      <code>127.0.0.1:{port}</code>
      <IconButton
        label={t("proxy_address.copy")}
        onClick={() =>
          void navigator.clipboard
            .writeText(`127.0.0.1:${port}`)
            .then(() => setCopied(true))
        }
      >
        <Copy size={15} />
      </IconButton>
      <span role="status">{copied ? t("proxy_address.copied") : ""}</span>
    </div>
  );
}
export function ConnectionStatus({
  status,
  hasServers,
  hasSubscriptions = false,
}: {
  status: string;
  hasServers: boolean;
  /** There are subscriptions: no servers then means they gave none, not that none was added. */
  hasSubscriptions?: boolean;
}) {
  const subtitle =
    status === "connected"
      ? ""
      : status === "connecting"
        ? t("connection_status.connecting")
        : status === "error"
          ? t("connection_status.error")
          : status === "lost"
            ? t("connection_status.lost")
            : hasServers
              ? t("connection_status.choose_server")
              : hasSubscriptions
                ? t("connection_status.subscription_empty", {
                    page: t("nav.servers"),
                  })
                : t("connection_status.add_server", { page: t("nav.servers") });
  return (
    <div className="connection-status" data-status={status}>
      <h2>
        <span className="status-dot" aria-hidden="true" />
        {connectionLabel(status)}
      </h2>
      <p role="status">{subtitle}</p>
    </div>
  );
}
/** Connection state for the pages that are not the connection page itself. */
export function LinkStatus({ status }: { status: string }) {
  return (
    <span className="link-status" data-status={status}>
      <span className="status-dot" aria-hidden="true" />
      {connectionLabel(status)}
    </span>
  );
}
