export type Mode = "tun" | "proxy" | "tun_proxy";
export interface Settings {
  mode: Mode;
  theme: "system" | "light" | "dark";
  /** The language of the interface: a code of the files of texts (`src/locales`). */
  language: string;
  auto_select: boolean;
  proxy_port: number;
  api_port: number;
  /** Seconds between fast checks of the best servers. */
  interval_secs: number;
  /** How many of the best servers are kept ready. */
  pool_size: number;
  /** Minutes between full checks, counted from the end of the last one. */
  full_interval_mins: number;
  /** A server of the pool is worse when its delay grew by this share, percent... */
  degrade_percent: number;
  /**
   * ...but the share never lifts the limit above this many milliseconds in total (a ceiling,
   * not an addition to the delay it had): a delay above it is allowed only the growth floor.
   */
  degrade_cap_ms: number;
  endpoint_timeout_ms: number;
  proxy_timeout_ms: number;
  concurrency: number;
  connectivity_url: string;
  subscription_interval_mins: number;
  /** A working server is left only for one faster by this share of its delay, percent... */
  switch_threshold_percent: number;
  /** ...and by this many milliseconds. */
  switch_threshold_ms: number;
  autostart: boolean;
  auto_connect: boolean;
  /** Look for new routing databases every twelve hours and fetch a new version. */
  geodata_auto_update: boolean;
  /** Ask sing-box which program opened each connection (shown on the connections page). */
  find_process: boolean;
}
export interface Server {
  id: string;
  name: string;
  protocol: string;
  source: string | null;
  endpoint_ms: number | null;
  proxy_ms: number | null;
  status: string;
  checked_at: number | null;
  failure_code: string | null;
  /** The server did not answer and is out of the cycles of selection: when it is asked again
   * (Unix seconds). Absent or null for a server that is measured with the others. */
  retry_at?: number | null;
  /** The user keeps the server out of Auto Select: it is shown last and grey. */
  excluded?: boolean;
}
/** One check of one server: its distance, its answer through it and why a stage failed. Kept in
 * memory by the core, asked for with `history`, never part of the snapshot. */
export interface HistoryRow {
  /** Unix seconds. */
  at: number;
  endpoint_ms: number | null;
  proxy_ms: number | null;
  failure_code: string | null;
}
/** Bytes per second through the proxy, once a second while the connection screen is open. */
export interface Speed {
  up: number;
  down: number;
}
/** One connection of the open connections page. Memory only: the core drops everything but these
 * fields and never keeps them. */
export interface ConnectionRow {
  host: string;
  port: number;
  network: "tcp" | "udp" | "other";
  /** `proxy`: through `server`; `direct`: no server. */
  route: "proxy" | "direct" | "other";
  server: string | null;
  /** The file name of the program, when sing-box looks programs up. */
  process: string | null;
  upload: number;
  download: number;
  seconds: number | null;
}
/** The newest connections and how many there are in all. */
export interface Connections {
  total: number;
  items: ConnectionRow[];
}
/** What happens to the traffic of a list. */
export type Route = "block" | "direct" | "proxy";
/** The routing profile: the order lists are checked in (the last takes the rest) and their entries. */
export interface Routing {
  order: [Route, Route, Route];
  block: string[];
  direct: string[];
  proxy: string[];
}
/** A geosite or geoip database on disk. */
export interface GeoDatabase {
  /** Unix seconds the file was written. */
  updated_at: number;
  bytes: number;
  /**
   * Unix seconds the source last confirmed it is the latest version. Kept beside the file, so a
   * restart still knows it; null when never confirmed.
   */
  checked_at?: number | null;
}
/** The databases behind `geosite:` and `geoip:` rules. */
export interface Geodata {
  geosite: GeoDatabase | null;
  geoip: GeoDatabase | null;
  updating: boolean;
  /** The database being fetched now: `geosite` or `geoip`. */
  current?: string | null;
  /** The routing profile refers to a list of this database. */
  uses_geosite?: boolean;
  uses_geoip?: boolean;
  /** Why the last download failed. */
  failure_code: string | null;
  /** This build can download them. */
  available: boolean;
  /** The routing profile refers to a list of the databases. */
  wanted: boolean;
}
/** Which servers of a subscription are kept at import. */
export interface SubscriptionFilters {
  /** Keep only servers whose name contains one of these (case-insensitive). */
  include: string[];
  exclude: string[];
  /** `vless`, `vmess`, `trojan`, `shadowsocks`; empty keeps all. */
  protocols: string[];
}
/** Traffic and term the provider reported at the last refresh. */
export interface SubscriptionUsage {
  /** Upload plus download, bytes. */
  used: number;
  /** Quota in bytes; `0`: none stated. */
  total: number;
  /** Unix seconds; `null`: no term stated. */
  expire: number | null;
}
export interface Subscription {
  id: string;
  /** From the subscription itself; the host of its URL until the first refresh. */
  name: string;
  count: number;
  updated_at: number | null;
  error: string | null;
  failure_code: string | null;
  /** Refresh interval in minutes; `null`: the global one. */
  interval_mins: number | null;
  /** `null`: Rift's own. */
  user_agent: string | null;
  filters: SubscriptionFilters;
  /** Whether the rules the provider sends with its servers (Xray JSON) are kept. */
  provider_routes: boolean;
  usage: SubscriptionUsage | null;
  /** Whether Auto Select may use its servers now. */
  availability: "ok" | "exhausted" | "expired";
}
/** One server link found in a text, with what it would add. */
export interface ServerPreview {
  /** The link as found: sent back to add the server. */
  uri: string;
  server: Server;
}
/** What a pasted text, a QR code or a `rift://` link asks Rift to import. Nothing is added yet. */
export type InputPreview =
  | { kind: "subscription"; url: string }
  | {
      kind: "servers";
      items: ServerPreview[];
      /** Links that no supported server could be read from. */
      rejected: number;
    };
/** Where a journal entry comes from. */
export type JournalCategory = "app" | "subscriptions" | "checks" | "core";
export interface Entry {
  at: number;
  level: string;
  message: string;
  /** Additional local details, bounded and redacted by Rust before persistence. */
  details?: string;
  /** Generated local crash identity; absent for ordinary and legacy records. */
  crash_id?: string;
  /** Always sent by Rust; entries written before the journal had sources read as `app`. */
  category: JournalCategory;
  /** Rust skips the key for an ordinary entry; the other views always send `null`. */
  failure_code?: string | null;
}
/** A release newer than the running version, found by the auto-updater. */
export interface UpdateInfo {
  version: string;
  notes: string | null;
  pub_date: string | null;
}
export interface ProfilePreview {
  request_id: number;
  version: number;
  servers: number;
  manual_servers: number;
  subscriptions: number;
  has_selected_server: boolean;
}
/** The columns the list of servers can be ordered by (the names of the core). */
export type SortColumn =
  | "name"
  | "protocol"
  | "endpoint_ms"
  | "proxy_ms"
  | "status"
  | "source"
  | "checked_at";
export interface ServersSort {
  column: SortColumn;
  descending: boolean;
}
export interface Snapshot {
  settings: Settings;
  servers: Server[];
  subscriptions: Subscription[];
  selected: string | null;
  status: string;
  runtime_active: boolean;
  checking: boolean;
  refreshing: boolean;
  pending: boolean;
  next_check: number | null;
  error: string | null;
  journal: Entry[];
  /** The welcome window is due: nothing is added and it was never answered. */
  welcome: boolean;
  routing: Routing;
  /** How the list of servers is ordered; saved by the core. */
  servers_sort: ServersSort;
  geodata: Geodata;
}
export type Action =
  | {
      type:
        | "connect"
        | "disconnect"
        | "check_now"
        | "clear_journal"
        | "export_diagnostics";
    }
  | { type: "set_auto"; enabled: boolean }
  | { type: "dismiss_welcome" }
  | { type: "set_servers_sort"; column: SortColumn; descending: boolean }
  | { type: "set_server_excluded"; id: string; excluded: boolean }
  | {
      type: "save_routing";
      order: [Route, Route, Route];
      /** One entry per line (commas also separate). */
      block: string;
      direct: string;
      proxy: string;
    }
  | { type: "reset_routing" }
  | { type: "update_geodata" }
  | { type: "select" | "remove_server" | "remove_subscription"; id: string }
  | { type: "add_server"; uri: string }
  | { type: "add_servers"; uris: string[] }
  | {
      type: "save_subscription";
      id: string | null;
      url: string;
      interval_mins: number | null;
      user_agent: string | null;
      filters: SubscriptionFilters;
      provider_routes: boolean;
    }
  | { type: "refresh"; id: string | null }
  | { type: "save_settings"; settings: Settings };
export interface BackendClient {
  snapshot(): Promise<Snapshot>;
  execute(action: Action): Promise<Snapshot>;
  preview(uri: string): Promise<Server>;
  /** Finds a subscription or server links in a text and checks them; rejects when it has none. */
  previewInput?(text: string): Promise<InputPreview>;
  /** Native file dialog, then the QR code of the picture; `null`: the dialog was canceled. The
   * path stays in Rust. Nothing is added. */
  scanQrFile?(): Promise<InputPreview | null>;
  /** The QR code of the picture on the clipboard. Nothing is added. */
  scanQrClipboard?(): Promise<InputPreview>;
  /** Pushed when a `rift://` link opens Rift: what it asks to import, not yet added. */
  onImport?(listener: (preview: InputPreview) => void): Promise<() => void>;
  /** The `rift://` link that started Rift, waiting for the page; the page listens from now on. */
  takeImport?(): Promise<InputPreview | null>;
  /** Native file dialog for a program; only its file name comes back, `null`: canceled. */
  pickApplication?(): Promise<string | null>;
  /** The latest measurements of one server (up to 20), the oldest first. */
  history?(id: string): Promise<HistoryRow[]>;
  /** The address of a subscription, for the dialog that edits it; it is nowhere else. */
  subscriptionUrl?(id: string): Promise<string>;
  /** The screen with the speed opened (`true`) or closed: Rust reads sing-box only in between. */
  watchTraffic?(on: boolean): Promise<void>;
  /** Pushed every second while watched; `null`: nothing is being read. Not part of the snapshot. */
  onTraffic?(listener: (speed: Speed | null) => void): Promise<() => void>;
  /** The connections page opened or closed: Rust asks sing-box only in between. */
  watchConnections?(on: boolean): Promise<void>;
  /** Pushed every two seconds while watched; `null`: nothing is being read. */
  onConnections?(
    listener: (connections: Connections | null) => void,
  ): Promise<() => void>;
  subscribe(listener: (s: Snapshot) => void): Promise<() => void>;
  /** Writes a local journal entry and toast. Rust bounds and redacts both strings. */
  logError(message: string, detail?: string): Promise<void>;
  /** Opens a native save dialog and writes the complete plaintext profile. */
  exportProfile(): Promise<boolean>;
  /** Opens and validates a profile without exposing its path or secrets to React. */
  inspectProfile(): Promise<ProfilePreview | null>;
  /** Applies the exact profile staged by `inspectProfile`. */
  importProfile(requestId: number): Promise<Snapshot>;
  /** First usable screen shown: startup milestone for the performance harness. */
  ready?(): Promise<void>;
  /** The real running version, e.g. for the brand bar. Optional like `ready`:
   * test doubles that don't care about updates need not implement this group. */
  appVersion?(): Promise<string>;
  /** Manual or periodic check; `null` when none found or auto-update is disabled. */
  checkForUpdate?(): Promise<UpdateInfo | null>;
  /** Stops the active connection and launches the installer. Rarely resolves on success:
   * the process exits once the installer is launched. */
  installUpdate?(): Promise<void>;
  /** Suppresses the banner for this version until a newer one appears. */
  dismissUpdate?(version: string): Promise<void>;
  /** Pushed when a periodic or manual check finds a version worth notifying about. */
  onUpdate?(listener: (info: UpdateInfo) => void): Promise<() => void>;
  /** Pushed for every error the host reports; shown as a toast. The text is the original
   * one (unscrubbed, local only): scrubbing applies to what leaves the machine. */
  onError?(listener: (message: string) => void): Promise<() => void>;
  /** Pushed for something the user should know that is no failure (a fallback that took
   * place); shown as an information toast, never reported. */
  onNotice?(listener: (message: string) => void): Promise<() => void>;
}
