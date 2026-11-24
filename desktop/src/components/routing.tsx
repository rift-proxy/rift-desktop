import { useState } from "react";
import { FolderOpen, RefreshCw } from "lucide-react";
import { useBackend } from "../backend/provider";
import type { Action, Geodata, Route, Routing } from "../backend/types";
import { bytes, dateText, failureText } from "../format";
import { Alert, Button, ConfirmationDialog, Select, TextArea } from "./ui";
import { t } from "../i18n";

type Order = [Route, Route, Route];
/** The six orders, as the core lists them. */
const ORDERS: Order[] = [
  ["block", "direct", "proxy"],
  ["block", "proxy", "direct"],
  ["direct", "block", "proxy"],
  ["direct", "proxy", "block"],
  ["proxy", "block", "direct"],
  ["proxy", "direct", "block"],
];
const title = (route: Route) => t(`route.${route}`);
const POSITIONS = [
  "routing.position.first",
  "routing.position.second",
  "routing.position.rest",
];
const orderLabel = (order: Order) => order.map(title).join(" → ");

interface Draft {
  order: Order;
  block: string;
  direct: string;
  proxy: string;
}
const toDraft = (routing: Routing): Draft => ({
  order: routing.order,
  block: routing.block.join("\n"),
  direct: routing.direct.join("\n"),
  proxy: routing.proxy.join("\n"),
});
const sameDraft = (a: Draft, b: Draft) =>
  JSON.stringify(a) === JSON.stringify(b);

/** What the databases look like to the user: loaded, loading, failed, not there. */
function databaseText(
  name: "geosite" | "geoip",
  database: Geodata["geosite"],
  geodata: Geodata,
): string {
  const fetching = geodata.current === name;
  if (database) {
    if (fetching) return t("routing.db.checking", { name });
    const loaded = t("routing.db.loaded", {
      name,
      date: dateText(database.updated_at),
      size: bytes(database.bytes),
    });
    return database.checked_at
      ? t("routing.db.latest", {
          loaded,
          date: dateText(database.checked_at),
        })
      : loaded;
  }
  if (fetching || (geodata.updating && !geodata.current))
    return t("routing.db.downloading", { name });
  if (geodata.updating) return t("routing.db.queued", { name });
  const used = name === "geosite" ? geodata.uses_geosite : geodata.uses_geoip;
  return used === false
    ? t("routing.db.not_loaded_unused", { name })
    : t("routing.db.not_loaded", { name });
}

export function RoutingScreen({
  routing,
  geodata,
  execute,
}: {
  routing: Routing;
  geodata: Geodata;
  execute: (a: Action) => Promise<boolean>;
}) {
  const api = useBackend();
  const [draft, setDraft] = useState(() => toDraft(routing));
  const [base, setBase] = useState(routing);
  // What the backend now holds replaces the draft only while nothing of it was edited: an
  // imported profile, a reset, a finished save.
  if (JSON.stringify(base) !== JSON.stringify(routing)) {
    setBase(routing);
    if (sameDraft(draft, toDraft(base))) setDraft(toDraft(routing));
  }
  const [busy, setBusy] = useState(false);
  const [resetting, setResetting] = useState(false);
  const changed = !sameDraft(draft, toDraft(routing));
  const rest = draft.order[2];
  const edit = (route: Route, text: string) =>
    setDraft((current) => ({ ...current, [route]: text }));
  const apply = () => {
    setBusy(true);
    void execute({
      type: "save_routing",
      order: draft.order,
      block: draft.block,
      direct: draft.direct,
      proxy: draft.proxy,
    }).finally(() => setBusy(false));
  };
  const addApplication = (route: Route) => {
    void api
      .pickApplication?.()
      ?.then((name) => {
        if (!name) return;
        setDraft((current) => ({
          ...current,
          [route]: [current[route].trimEnd(), `process:${name}`]
            .filter(Boolean)
            .join("\n"),
        }));
      })
      .catch(() => {});
  };
  const failure = failureText(
    geodata.failure_code,
    t("routing.databases_failed"),
  );
  return (
    <section className="page page-form">
      <div className="page-head">
        <div>
          <h1>{t("nav.routing")}</h1>
          <p className="muted small">{t("routing.intro")}</p>
        </div>
      </div>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          apply();
        }}
      >
        <section className="group">
          <Select
            label={t("routing.order")}
            value={orderLabel(draft.order)}
            onChange={(event) => {
              const order = ORDERS.find(
                (candidate) => orderLabel(candidate) === event.target.value,
              );
              if (order) setDraft((current) => ({ ...current, order }));
            }}
          >
            {ORDERS.map((order) => (
              <option key={orderLabel(order)} value={orderLabel(order)}>
                {orderLabel(order)}
              </option>
            ))}
          </Select>
          {rest === "block" && (
            <Alert tone="info">
              {t("routing.block_last", { route: title("block") })}
            </Alert>
          )}
        </section>
        {draft.order.map((route, position) => {
          const last = position === 2;
          return (
            <section className="group" key={route}>
              <h2>
                {title(route)}{" "}
                <span className="muted small">
                  {t(POSITIONS[position])} · {t(`routing.meaning.${route}`)}
                </span>
              </h2>
              <TextArea
                label={t("routing.rules_label", { route: title(route) })}
                value={draft[route]}
                disabled={last}
                rows={last ? 3 : 8}
                placeholder={
                  last
                    ? t("routing.rest_placeholder")
                    : t("routing.rules_placeholder")
                }
                spellCheck={false}
                onChange={(event) => edit(route, event.target.value)}
              />
              {last ? (
                <p className="muted small">{t("routing.rest_note")}</p>
              ) : (
                <Button
                  type="button"
                  variant="ghost"
                  onClick={() => addApplication(route)}
                >
                  <FolderOpen size={15} />
                  {t("routing.add_application")}
                </Button>
              )}
            </section>
          );
        })}
        <div className="routing-actions">
          <Button
            type="submit"
            variant="primary"
            loading={busy}
            disabled={!changed}
          >
            {t("routing.apply")}
          </Button>
          <Button
            type="button"
            disabled={!changed || busy}
            onClick={() => setDraft(toDraft(routing))}
          >
            {t("routing.discard")}
          </Button>
          <Button
            type="button"
            variant="ghost"
            onClick={() => setResetting(true)}
          >
            {t("routing.reset")}
          </Button>
        </div>
        <p className="muted small">{t("routing.apply_note")}</p>
      </form>
      <section className="group">
        <h2>{t("routing.databases")}</h2>
        <p>{databaseText("geosite", geodata.geosite, geodata)}</p>
        <p>{databaseText("geoip", geodata.geoip, geodata)}</p>
        {geodata.wanted && !geodata.geosite && !geodata.geoip && (
          <p className="muted small">{t("routing.databases_unused")}</p>
        )}
        {failure && <Alert>{failure}</Alert>}
        <Button
          type="button"
          loading={geodata.updating}
          disabled={!geodata.available}
          onClick={() => void execute({ type: "update_geodata" })}
        >
          <RefreshCw size={14} />
          {t("routing.update_databases")}
        </Button>
        <p className="muted small">{t("routing.update_note")}</p>
        {!geodata.available && (
          <p className="muted small">{t("routing.downloads_unavailable")}</p>
        )}
      </section>
      <details>
        <summary>{t("routing.syntax_title")}</summary>
        <table className="syntax">
          <tbody>
            {[
              ["domain:example.com", "domain"],
              ["full:www.example.com", "full"],
              ["keyword:ads", "keyword"],
              ["regexp:^ad[0-9]+\\.", "regexp"],
              ["geosite:category-ru", "geosite"],
              ["10.0.0.0/8, 192.0.2.1, fc00::/7", "address"],
              ["geoip:ru", "geoip"],
              ["port:443, port:1000-2000", "port"],
              ["udp:443, tcp:1000-2000", "port_protocol"],
              ["protocol:bittorrent", "protocol"],
              ["process:chrome.exe", "process"],
            ].map(([entry, meaning]) => (
              <tr key={entry}>
                <td>
                  <code>{entry}</code>
                </td>
                <td>{t(`routing.syntax.${meaning}`)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="muted small">{t("routing.syntax_note")}</p>
      </details>
      {resetting && (
        <ConfirmationDialog
          title={t("routing.reset_title")}
          message={t("routing.reset_message")}
          confirmLabel={t("routing.reset_confirm")}
          confirmVariant="primary"
          onClose={() => setResetting(false)}
          onConfirm={() =>
            void execute({ type: "reset_routing" }).then((ok) => {
              if (ok) setResetting(false);
            })
          }
        />
      )}
    </section>
  );
}
