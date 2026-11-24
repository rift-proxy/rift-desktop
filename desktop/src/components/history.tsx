import { useEffect, useState } from "react";
import { useBackend } from "../backend/provider";
import type { HistoryRow, Server } from "../backend/types";
import { failureText, latency, time } from "../format";
import { Select } from "./ui";
import { t } from "../i18n";
import { describe } from "./errors";

/** The latest measurements of one server, asked from the core when the server is checked again.
 * They live in memory only and are not part of the snapshot. */
export function useServerHistory(
  id: string | null,
  checkedAt: number | null,
): HistoryRow[] {
  const api = useBackend();
  const [rows, setRows] = useState<HistoryRow[]>([]);
  useEffect(() => {
    if (id === null || !api.history) {
      setRows([]);
      return;
    }
    let current = true;
    api
      .history(id)
      .then((next) => {
        if (current) setRows(next);
      })
      .catch((error: unknown) => {
        if (current) {
          setRows([]);
          void api.logError(t("history.load_failed"), describe(error));
        }
      });
    return () => {
      current = false;
    };
  }, [api, id, checkedAt]);
  return rows;
}

function outcome(row: HistoryRow): string {
  if (row.failure_code)
    return (
      failureText(row.failure_code, t("history.check_failed")) ??
      t("history.check_failed")
    );
  return row.proxy_ms === null
    ? t("history.only_to_server")
    : t("history.success");
}
/** The newest row first. */
export function HistoryTable({ rows }: { rows: HistoryRow[] }) {
  if (rows.length === 0)
    return <p className="muted small">{t("history.empty")}</p>;
  return (
    <div className="table-scroll">
      <table className="history">
        <caption className="sr-only">{t("history.caption")}</caption>
        <thead>
          <tr>
            <th>{t("history.time")}</th>
            <th>{t("history.to_server")}</th>
            <th>{t("history.via_server")}</th>
            <th>{t("history.result")}</th>
          </tr>
        </thead>
        <tbody>
          {[...rows].reverse().map((row, index) => (
            <tr key={`${row.at}-${index}`}>
              <td>{time(row.at)}</td>
              <td>{latency(row.endpoint_ms)}</td>
              <td>{latency(row.proxy_ms)}</td>
              <td className={row.failure_code ? "status-reason" : undefined}>
                {outcome(row)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
/** Measurements of the server in use, for the details of the connection. */
export function ConnectionHistory({ server }: { server: Server | undefined }) {
  const rows = useServerHistory(server?.id ?? null, server?.checked_at ?? null);
  return (
    <div className="fact">
      <div className="fact-label">{t("history.latest")}</div>
      <HistoryTable rows={rows} />
    </div>
  );
}
/** Measurements of any server, for the journal of the checks. */
export function HistoryPanel({
  servers,
  selected,
}: {
  servers: Server[];
  selected: string | null;
}) {
  const [chosen, setChosen] = useState<string | null>(null);
  const names = [...servers].sort((a, b) => a.name.localeCompare(b.name));
  const id =
    names.find((s) => s.id === chosen)?.id ??
    names.find((s) => s.id === selected)?.id ??
    names[0]?.id ??
    null;
  const server = servers.find((s) => s.id === id);
  const rows = useServerHistory(id, server?.checked_at ?? null);
  if (servers.length === 0) return null;
  return (
    <details className="history-panel">
      <summary>{t("history.panel_title")}</summary>
      <Select
        label={t("history.server")}
        value={id ?? ""}
        onChange={(e) => setChosen(e.target.value)}
      >
        {names.map((s) => (
          <option key={s.id} value={s.id}>
            {s.name}
          </option>
        ))}
      </Select>
      <HistoryTable rows={rows} />
    </details>
  );
}
