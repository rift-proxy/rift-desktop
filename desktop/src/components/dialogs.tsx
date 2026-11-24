import { useEffect, useRef, useState } from "react";
import type {
  Action,
  InputPreview,
  Server,
  ServerPreview,
  Subscription,
} from "../backend/types";
import { useBackend } from "../backend/provider";
import { Dialog, Field, Button, Alert, Badge, Toggle, Checkbox } from "./ui";
import { t } from "../i18n";
/** How many names of a batch the dialog lists; the rest is only counted. */
const LISTED_SERVERS = 6;
export function AddServerDialog({
  incoming,
  onClose,
  execute,
  submitError,
}: {
  /** Servers found in a paste, a QR code or a `rift://` link: already checked, shown at once. */
  incoming?: { items: ServerPreview[]; rejected: number };
  onClose: () => void;
  execute: (a: Action) => Promise<boolean>;
  submitError?: string | null;
}) {
  const api = useBackend();
  const [uri, setUri] = useState("");
  const [checked, setChecked] = useState<{
    uri: string;
    server: Server;
  } | null>(null);
  const parsed = checked?.uri === uri ? checked.server : null;
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  // Invalidate in the input event, before React renders: even a queued click cannot add an
  // earlier preview under a newer link. Generation also distinguishes A → B → A requests.
  const request = useRef({
    generation: 0,
    uri: "",
    accepted: null as string | null,
  });
  const timer = useRef<number | undefined>(undefined);
  useEffect(
    () => () => {
      clearTimeout(timer.current);
      request.current.generation += 1;
    },
    [incoming],
  );
  const preview = (text: string) => {
    const generation = ++request.current.generation;
    clearTimeout(timer.current);
    request.current.uri = text;
    request.current.accepted = null;
    setUri(text);
    setChecked(null);
    setError(null);
    setLoading(Boolean(text.trim()));
    if (!text.trim()) return;
    timer.current = window.setTimeout(() => {
      void api.preview(text).then(
        (server) => {
          if (request.current.generation !== generation) return;
          request.current.accepted = text;
          setChecked({ uri: text, server });
          setLoading(false);
        },
        (problem: unknown) => {
          if (request.current.generation !== generation) return;
          setError(
            typeof problem === "string"
              ? problem
              : t("add_server_dialog.parse_failed"),
          );
          setLoading(false);
        },
      );
    }, 300);
  };
  if (incoming) {
    const { items, rejected } = incoming;
    return (
      <Dialog title={t("add_servers_dialog.title")} onClose={onClose}>
        {submitError && <Alert>{submitError}</Alert>}
        <div className="preview">
          <h3>
            {items.length === 1
              ? items[0].server.name
              : t("add_servers_dialog.found", { count: items.length })}
          </h3>
          {items.length === 1 && <Badge>{items[0].server.protocol}</Badge>}
        </div>
        {items.length > 1 && (
          <ul className="incoming-list">
            {items.slice(0, LISTED_SERVERS).map((item) => (
              <li key={item.uri}>
                {item.server.name} <Badge>{item.server.protocol}</Badge>
              </li>
            ))}
            {items.length > LISTED_SERVERS && (
              <li className="muted">
                {t("add_servers_dialog.and_more", {
                  count: items.length - LISTED_SERVERS,
                })}
              </li>
            )}
          </ul>
        )}
        {rejected > 0 && (
          <p className="muted small">
            {t("add_servers_dialog.rejected", { count: rejected })}
          </p>
        )}
        <p className="muted small">{t("add_servers_dialog.confirm_note")}</p>
        <div className="dialog-actions">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            onClick={() =>
              void execute({
                type: "add_servers",
                uris: items.map((item) => item.uri),
              }).then((ok) => {
                if (ok) onClose();
              })
            }
          >
            {items.length === 1
              ? t("common.add")
              : t("add_servers_dialog.add_many", { count: items.length })}
          </Button>
        </div>
      </Dialog>
    );
  }
  return (
    <Dialog title={t("add_server_dialog.title")} onClose={onClose}>
      {submitError && <Alert>{submitError}</Alert>}
      <Field
        label={t("add_server_dialog.uri_label")}
        placeholder={t("add_server_dialog.uri_placeholder")}
        value={uri}
        onChange={(event) => preview(event.target.value)}
      />
      {loading && (
        <p className="muted small" role="status">
          {t("add_server_dialog.parsing")}
        </p>
      )}
      {error && <Alert>{error}</Alert>}
      {parsed ? (
        <div className="preview">
          <h3>{parsed.name}</h3>
          <Badge>{parsed.protocol}</Badge>
          <p>{t("add_server_dialog.parsed")}</p>
        </div>
      ) : (
        <p className="muted small">
          {t("add_server_dialog.hint", { page: t("nav.servers") })}
        </p>
      )}
      <div className="dialog-actions">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button
          variant="primary"
          disabled={!parsed}
          onClick={() => {
            const current = request.current;
            if (current.accepted === null || current.accepted !== current.uri)
              return;
            void execute({ type: "add_server", uri: current.uri }).then(
              (ok) => {
                if (ok) onClose();
              },
            );
          }}
        >
          {t("common.add")}
        </Button>
      </div>
    </Dialog>
  );
}
const PROTOCOLS: [string, string][] = [
  ["vless", "VLESS"],
  ["vmess", "VMess"],
  ["trojan", "Trojan"],
  ["shadowsocks", "Shadowsocks"],
];
/** `DE, NL` or one per line, as the list it means. */
function entries(text: string): string[] {
  return text
    .split(/[,\n]/)
    .map((entry) => entry.trim())
    .filter(Boolean);
}
export function SubscriptionDialog({
  item,
  initialUrl = "",
  onClose,
  execute,
  submitError,
}: {
  item?: Subscription;
  /** A link taken from a paste, a QR code or a deep link. */
  initialUrl?: string;
  onClose: () => void;
  execute: (a: Action) => Promise<boolean>;
  submitError?: string | null;
}) {
  const api = useBackend();
  const [url, setUrl] = useState(initialUrl);
  // The address of a subscription that is edited is asked of the core: it is in no snapshot. Until
  // it comes the field is held, so that an empty field is not mistaken for an address.
  const [reading, setReading] = useState(
    Boolean(item && api.subscriptionUrl && !initialUrl),
  );
  const [readError, setReadError] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const wasReading = useRef(reading);
  // The field is held (disabled) while the address is read, and a disabled field cannot take the
  // focus the dialog gives to its first field when it opens: it stays on the close icon. When
  // the read ends, the field gets it, also after a failure (the field is then where the address
  // is typed). Not when the user has gone to a control of the form meanwhile.
  useEffect(() => {
    if (wasReading.current && !reading) {
      const element = form.current;
      if (element && !element.contains(document.activeElement)) {
        element.querySelector<HTMLElement>('input[type="url"]')?.focus();
      }
    }
    wasReading.current = reading;
  }, [reading]);
  useEffect(() => {
    if (!item || !api.subscriptionUrl || initialUrl) return;
    let live = true;
    api
      .subscriptionUrl(item.id)
      .then((address) => {
        if (!live) return;
        setUrl((typed) => typed || address);
        setReading(false);
      })
      .catch(() => {
        if (!live) return;
        setReadError(t("subscription_dialog.read_failed"));
        setReading(false);
      });
    return () => {
      live = false;
    };
  }, [api, item, initialUrl]);
  const [interval, setInterval] = useState(
    item?.interval_mins ? String(item.interval_mins) : "",
  );
  const [agent, setAgent] = useState(item?.user_agent ?? "");
  const [include, setInclude] = useState(
    item?.filters.include.join(", ") ?? "",
  );
  const [exclude, setExclude] = useState(
    item?.filters.exclude.join(", ") ?? "",
  );
  const [protocols, setProtocols] = useState<string[]>(
    item?.filters.protocols ?? [],
  );
  const [routes, setRoutes] = useState(item?.provider_routes ?? true);
  const customized = Boolean(
    item &&
    (item.interval_mins ||
      item.user_agent ||
      !item.provider_routes ||
      item.filters.include.length ||
      item.filters.exclude.length ||
      item.filters.protocols.length),
  );
  return (
    <Dialog
      title={t(
        item
          ? "subscription_dialog.title_edit"
          : "subscription_dialog.title_add",
      )}
      onClose={onClose}
    >
      {submitError && <Alert>{submitError}</Alert>}
      {readError && <Alert>{readError}</Alert>}
      <form
        ref={form}
        onSubmit={(e) => {
          e.preventDefault();
          if (reading) return;
          void execute({
            type: "save_subscription",
            id: item?.id ?? null,
            url,
            interval_mins: interval.trim() ? Number(interval) : null,
            user_agent: agent.trim() ? agent.trim() : null,
            filters: {
              include: entries(include),
              exclude: entries(exclude),
              protocols,
            },
            provider_routes: routes,
          }).then((ok) => {
            if (ok) onClose();
          });
        }}
      >
        <Field
          label={t("subscription_dialog.url_label")}
          type="url"
          placeholder={t(
            reading
              ? "subscription_dialog.reading"
              : "subscription_dialog.url_placeholder",
          )}
          value={url}
          required
          disabled={reading}
          onChange={(e) => setUrl(e.target.value)}
        />
        <p className="muted small">{t("subscription_dialog.name_note")}</p>
        <details open={customized}>
          <summary>{t("common.advanced")}</summary>
          <Field
            label={t("subscription_dialog.interval_label")}
            type="number"
            min={1}
            max={10080}
            placeholder={t("subscription_dialog.interval_placeholder")}
            value={interval}
            onChange={(e) => setInterval(e.target.value)}
          />
          <Field
            label={t("subscription_dialog.user_agent")}
            maxLength={256}
            placeholder={t("subscription_dialog.user_agent_placeholder")}
            value={agent}
            onChange={(e) => setAgent(e.target.value)}
          />
          <Field
            label={t("subscription_dialog.include_label")}
            placeholder={t("subscription_dialog.include_placeholder")}
            value={include}
            onChange={(e) => setInclude(e.target.value)}
          />
          <Field
            label={t("subscription_dialog.exclude_label")}
            placeholder={t("subscription_dialog.exclude_placeholder")}
            value={exclude}
            onChange={(e) => setExclude(e.target.value)}
          />
          <fieldset className="choices">
            <legend>{t("subscription_dialog.protocols_legend")}</legend>
            {PROTOCOLS.map(([value, label]) => (
              <Checkbox
                key={value}
                label={label}
                checked={protocols.includes(value)}
                onChange={(e) =>
                  setProtocols(
                    e.target.checked
                      ? [...protocols, value]
                      : protocols.filter((p) => p !== value),
                  )
                }
              />
            ))}
          </fieldset>
          <p className="muted small">{t("subscription_dialog.filters_note")}</p>
          <Toggle
            label={t("subscription_dialog.provider_routes")}
            checked={routes}
            onChange={setRoutes}
          />
          <p className="muted small">
            {t("subscription_dialog.provider_routes_hint")}
          </p>
        </details>
        <div className="dialog-actions">
          <Button type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" variant="primary">
            {item ? t("common.save") : t("common.add")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

/** What the pasted text turned out to be, in a few words. */
function incomingSummary(preview: InputPreview): string {
  if (preview.kind === "subscription") {
    try {
      return t("welcome.subscription_host", {
        host: new URL(preview.url).hostname,
      });
    } catch {
      return t("welcome.subscription");
    }
  }
  const { items, rejected } = preview;
  const found =
    items.length === 1
      ? t("welcome.server", { name: items[0].server.name })
      : t("welcome.found", { count: items.length });
  return rejected > 0
    ? t("welcome.with_rejected", { found, count: rejected })
    : found;
}
/** The first start: one field for a subscription or a server link, or skip. */
export function WelcomeDialog({
  execute,
  submitError,
}: {
  execute: (a: Action) => Promise<boolean>;
  submitError?: string | null;
}) {
  const api = useBackend();
  const [text, setText] = useState("");
  const [checked, setChecked] = useState<{
    text: string;
    preview: InputPreview;
  } | null>(null);
  const preview = checked?.text === text ? checked.preview : null;
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // What is typed or pasted is read in Rust after a short pause; the type is not guessed here.
  useEffect(() => {
    if (!text.trim()) {
      setChecked(null);
      setProblem(null);
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      void api
        .previewInput?.(text)
        .then((found) => {
          if (!current) return;
          setChecked({ text, preview: found });
          setProblem(null);
        })
        .catch((error: unknown) => {
          if (!current) return;
          setChecked(null);
          setProblem(
            typeof error === "string" ? error : t("welcome.nothing_found"),
          );
        });
    }, 300);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [text, api]);
  const skip = () => {
    setBusy(true);
    void execute({ type: "dismiss_welcome" }).finally(() => setBusy(false));
  };
  const add = () => {
    if (!preview) return;
    setBusy(true);
    // Adding anything ends the welcome by itself: the dialog goes away with the empty state.
    void execute(
      preview.kind === "subscription"
        ? {
            type: "save_subscription",
            id: null,
            url: preview.url,
            interval_mins: null,
            user_agent: null,
            filters: { include: [], exclude: [], protocols: [] },
            provider_routes: true,
          }
        : { type: "add_servers", uris: preview.items.map((item) => item.uri) },
    ).finally(() => setBusy(false));
  };
  return (
    <Dialog
      title={t("welcome.title")}
      onClose={skip}
      closeDisabled={busy}
      hideClose
    >
      {submitError && <Alert>{submitError}</Alert>}
      <p>{t("welcome.intro", { page: t("nav.servers") })}</p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
      >
        <Field
          label={t("welcome.field")}
          placeholder={t("welcome.placeholder")}
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
        {preview && (
          <p className="welcome-found" role="status">
            {incomingSummary(preview)}
          </p>
        )}
        {problem && <Alert>{problem}</Alert>}
        <div className="dialog-actions">
          <Button type="button" onClick={skip} disabled={busy}>
            {t("common.skip")}
          </Button>
          <Button
            type="submit"
            variant="primary"
            disabled={!preview}
            loading={busy}
          >
            {t("common.add")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
