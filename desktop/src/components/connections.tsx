import { useEffect, useState, type ReactNode } from "react";
import { ArrowDown, ArrowUp, ChevronDown, ChevronUp } from "lucide-react";
import { useBackend } from "../backend/provider";
import type { ConnectionRow, Connections, Snapshot } from "../backend/types";
import { bytes, durationText } from "../format";
import { Badge, EmptyState, Field, LoadingState } from "./ui";
import { t } from "../i18n";

/** The connections while this page is open and Rift is connected; the core asks sing-box only in
 * that time and keeps nothing. `undefined`: not read yet. */
function useConnections(connected: boolean): Connections | undefined {
  const api = useBackend();
  const [list, setList] = useState<Connections | undefined>(undefined);
  useEffect(() => {
    if (!connected || !api.watchConnections || !api.onConnections) {
      setList(undefined);
      return;
    }
    let active = true;
    let dispose: (() => void) | undefined;
    const listener = (next: Connections | null) => {
      if (active) setList(next ?? undefined);
    };
    void api.onConnections(listener).then((fn) => {
      if (active) {
        dispose = fn;
        void api.watchConnections?.(true);
      } else fn();
    });
    return () => {
      active = false;
      dispose?.();
      void api.watchConnections?.(false);
      setList(undefined);
    };
  }, [api, connected]);
  return list;
}

const NETWORK: Record<ConnectionRow["network"], string> = {
  tcp: "TCP",
  udp: "UDP",
  other: "—",
};
function routeText(row: ConnectionRow): string {
  if (row.route === "direct") return t("connections.direct");
  if (row.route === "proxy") return row.server ?? t("connections.via_server");
  return "—";
}
/** The columns of the table the rows can be ordered by. */
type Column =
  "host" | "network" | "route" | "process" | "download" | "upload" | "seconds";
/** The value a row is ordered by in a column; `null`: it has none and is last either way. */
function sortKey(row: ConnectionRow, column: Column): string | number | null {
  switch (column) {
    case "host":
      return row.host || null;
    case "network":
      return NETWORK[row.network];
    case "route":
      return routeText(row);
    case "process":
      return row.process;
    case "download":
      return row.download;
    case "upload":
      return row.upload;
    case "seconds":
      return row.seconds;
  }
}
/** The rows ordered by a column; equal rows and the ones without a value keep the order they came in. */
function ordered(
  rows: ConnectionRow[],
  sort: { column: Column; descending: boolean } | null,
): ConnectionRow[] {
  if (!sort) return rows;
  return rows
    .map((row, index) => ({ row, index, key: sortKey(row, sort.column) }))
    .sort((a, b) => {
      if (a.key === b.key) return a.index - b.index;
      if (a.key === null) return 1;
      if (b.key === null) return -1;
      const order =
        typeof a.key === "string"
          ? a.key.localeCompare(String(b.key), "ru")
          : a.key - Number(b.key);
      return order === 0 ? a.index - b.index : sort.descending ? -order : order;
    })
    .map(({ row }) => row);
}
/** A heading that orders the rows by its column and says which way, as in the list of servers. */
function SortHeading({
  column,
  sort,
  onSort,
  children,
  chevrons = false,
}: {
  column: Column;
  sort: { column: Column; descending: boolean } | null;
  onSort: (column: Column) => void;
  children: ReactNode;
  /** The heading already is an arrow (received, sent): the way of the order is a chevron. */
  chevrons?: boolean;
}) {
  const Up = chevrons ? ChevronUp : ArrowUp;
  const Down = chevrons ? ChevronDown : ArrowDown;
  const active = sort?.column === column;
  return (
    <th
      aria-sort={
        active ? (sort.descending ? "descending" : "ascending") : undefined
      }
    >
      <button type="button" className="sort" onClick={() => onSort(column)}>
        {children}
        {active &&
          (sort.descending ? (
            <Down className="sort-arrow" size={12} aria-hidden="true" />
          ) : (
            <Up className="sort-arrow" size={12} aria-hidden="true" />
          ))}
      </button>
    </th>
  );
}
function matches(row: ConnectionRow, search: string): boolean {
  const needle = search.trim().toLocaleLowerCase();
  if (!needle) return true;
  return [row.host, routeText(row), row.process ?? "", NETWORK[row.network]]
    .join("\n")
    .toLocaleLowerCase()
    .includes(needle);
}

export function ConnectionsScreen({ state }: { state: Snapshot }) {
  const list = useConnections(state.runtime_active);
  const [search, setSearch] = useState("");
  // Not saved: the order lasts while the page is open, through the refreshes of the list.
  const [sort, setSort] = useState<{
    column: Column;
    descending: boolean;
  } | null>(null);
  const onSort = (column: Column) =>
    setSort((now) =>
      now?.column !== column
        ? { column, descending: false }
        : now.descending
          ? null
          : { column, descending: true },
    );
  const rows = ordered(
    list?.items.filter((row) => matches(row, search)) ?? [],
    sort,
  );
  return (
    <section className="page" aria-label={t("nav.connections")}>
      <div className="page-head">
        <h1>{t("nav.connections")}</h1>
      </div>
      <p className="muted small page-note">{t("connections.note")}</p>
      {!state.runtime_active ? (
        <EmptyState
          level={2}
          title={t("connections.not_connected_title")}
          description={t("connections.not_connected_description", {
            page: t("nav.connection"),
          })}
        />
      ) : list === undefined ? (
        <LoadingState label={t("connections.loading")} />
      ) : list.items.length === 0 ? (
        <EmptyState
          level={2}
          title={t("connections.empty_title")}
          description={t("connections.empty_description")}
        />
      ) : (
        <>
          <div className="toolbar">
            <Field
              label={t("connections.search")}
              type="search"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
          {list.total > list.items.length && (
            <p className="muted small" role="status">
              {t("connections.showing_newest", {
                shown: list.items.length,
                total: list.total,
              })}
            </p>
          )}
          {rows.length === 0 ? (
            <EmptyState
              level={2}
              title={t("connections.nothing_found_title")}
              description={t("connections.nothing_found_description")}
            />
          ) : (
            <div className="table-scroll">
              <table className="connections">
                <thead>
                  <tr>
                    <SortHeading column="host" sort={sort} onSort={onSort}>
                      {t("connections.host")}
                    </SortHeading>
                    <SortHeading column="network" sort={sort} onSort={onSort}>
                      {t("connections.network")}
                    </SortHeading>
                    <SortHeading column="route" sort={sort} onSort={onSort}>
                      {t("connections.route")}
                    </SortHeading>
                    <SortHeading column="process" sort={sort} onSort={onSort}>
                      {t("connections.process")}
                    </SortHeading>
                    <SortHeading
                      column="download"
                      sort={sort}
                      onSort={onSort}
                      chevrons
                    >
                      <ArrowDown size={13} aria-hidden="true" />
                      <span className="sr-only">
                        {t("connections.received")}
                      </span>
                    </SortHeading>
                    <SortHeading
                      column="upload"
                      sort={sort}
                      onSort={onSort}
                      chevrons
                    >
                      <ArrowUp size={13} aria-hidden="true" />
                      <span className="sr-only">{t("connections.sent")}</span>
                    </SortHeading>
                    <SortHeading column="seconds" sort={sort} onSort={onSort}>
                      {t("connections.duration")}
                    </SortHeading>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row, index) => (
                    <tr key={`${index}-${row.host}-${row.port}`}>
                      <td className="connection-host">
                        {row.host || "—"}
                        {row.port > 0 && (
                          <span className="muted">:{row.port}</span>
                        )}
                      </td>
                      <td>
                        <Badge>{NETWORK[row.network]}</Badge>
                      </td>
                      <td>{routeText(row)}</td>
                      <td>{row.process ?? "—"}</td>
                      <td>{bytes(row.download)}</td>
                      <td>{bytes(row.upload)}</td>
                      <td>
                        {row.seconds === null ? "—" : durationText(row.seconds)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {list.items.every((row) => row.process === null) && (
            <p className="muted small">
              {t("connections.process_unknown", {
                setting: t("settings.find_process.label"),
              })}
            </p>
          )}
        </>
      )}
    </section>
  );
}
