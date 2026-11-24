import type {
  Server,
  ServersSort,
  Settings,
  SortColumn,
  Subscription,
} from "./backend/types";
import { t } from "./i18n";

/** What a new installation, and the third click on a column, order by. */
export const DEFAULT_SORT: ServersSort = { column: "name", descending: false };

/** Best first, after the server in use: those that answer, then those that do not. */
const STATUS_ORDER = [
  "online",
  "reachable",
  "unchecked",
  "failed",
  "unreachable",
];

/** The value a column is ordered by; `null` is a server that has none yet. */
function sortKey(
  server: Server,
  column: SortColumn,
  sources: Map<string | null, string>,
  selected: string | null,
): string | number | null {
  switch (column) {
    case "name":
      return server.name;
    case "protocol":
      return server.protocol;
    case "endpoint_ms":
      return server.endpoint_ms;
    case "proxy_ms":
      return server.proxy_ms;
    case "status": {
      // The server in use is the best of them, whatever its last check said.
      if (server.id === selected) return -1;
      const rank = STATUS_ORDER.indexOf(server.status);
      return rank < 0 ? STATUS_ORDER.length : rank;
    }
    case "source":
      // A manual server has no source; so has one whose subscription is gone.
      return sources.get(server.source) ?? t("servers.manual");
    case "checked_at":
      return server.checked_at;
  }
}

/**
 * The servers in the order the user chose. A server without a value (not measured, not checked)
 * is last in either direction; equal values go by name, so the order never jumps. The servers
 * kept out of Auto Select come after all the others and are ordered among themselves.
 */
export function sortServers(
  servers: Server[],
  sources: Subscription[],
  sort: ServersSort,
  selected: string | null = null,
): Server[] {
  const names = new Map<string | null, string>(
    sources.map((s): [string, string] => [s.id, s.name]),
  );
  return [...servers].sort((a, b) => {
    // What the user keeps out of Auto Select is last, whatever the column and the direction.
    if (Boolean(a.excluded) !== Boolean(b.excluded)) return a.excluded ? 1 : -1;
    const x = sortKey(a, sort.column, names, selected);
    const y = sortKey(b, sort.column, names, selected);
    if (x !== y) {
      if (x === null) return 1;
      if (y === null) return -1;
      // One column holds one kind of value, so `y` is a string when `x` is.
      const order =
        typeof x === "string"
          ? x.localeCompare(String(y), "ru")
          : x - Number(y);
      if (order !== 0) return sort.descending ? -order : order;
    }
    return a.name.localeCompare(b.name, "ru") || a.id.localeCompare(b.id);
  });
}

/** What a click on a column heading orders by: it, then backwards, then back to the name. */
export function nextSort(sort: ServersSort, column: SortColumn): ServersSort {
  if (sort.column !== column) return { column, descending: false };
  return sort.descending ? DEFAULT_SORT : { column, descending: true };
}

export function visibleServers(
  servers: Server[],
  source: string,
  sources: Subscription[],
  sort: ServersSort,
  selected: string | null = null,
) {
  return sortServers(
    servers.filter(
      (s) =>
        source === "all" ||
        (source === "manual" ? s.source === null : s.source === source),
    ),
    sources,
    sort,
    selected,
  );
}

export type SettingsProblems = Partial<Record<keyof Settings, string>>;

const RANGES = [
  ["proxy_port", 1, 65535],
  ["api_port", 1, 65535],
  ["interval_secs", 50, 300],
  ["pool_size", 1, 5],
  ["full_interval_mins", 10, 30],
  ["degrade_percent", 10, 300],
  ["degrade_cap_ms", 200, 10000],
  ["endpoint_timeout_ms", 100, 30000],
  ["proxy_timeout_ms", 100, 30000],
  ["concurrency", 1, 3],
  ["subscription_interval_mins", 1, 10080],
  ["switch_threshold_percent", 0, 100],
  ["switch_threshold_ms", 0, 10000],
] as const;

function isPlainHttps(text: string): boolean {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return false;
  }
  return (
    url.protocol === "https:" && url.username === "" && url.password === ""
  );
}

/** Why a settings draft must not be sent yet. It only keeps an obviously wrong value (a port
 * half typed, an empty field) away from the backend; `Settings::validate` in the core stays
 * the authority and rejects anything this lets through. */
export function settingsProblems(settings: Settings): SettingsProblems {
  const problems: SettingsProblems = {};
  for (const [key, min, max] of RANGES) {
    const value = settings[key];
    if (!Number.isInteger(value) || value < min || value > max) {
      problems[key] = t("settings_problem.range", { min, max });
    }
  }
  if (!problems.api_port && settings.proxy_port === settings.api_port) {
    problems.api_port = t("settings_problem.api_port_differs");
  }
  if (!isPlainHttps(settings.connectivity_url)) {
    problems.connectivity_url = t("settings_problem.https_url");
  }
  return problems;
}
