// Only imported by Storybook and tests. Never by main.tsx.
import desktopPackage from "../../package.json" with { type: "json" };

import type {
  BackendClient,
  Connections,
  Snapshot,
  Action,
  HistoryRow,
  InputPreview,
  Server,
  Speed,
  UpdateInfo,
} from "../backend/types";
const release = /^(\d+)\.(\d+)\.(\d+)(?:-[\w.-]+)?(?:\+[\w.-]+)?$/.exec(
  desktopPackage.version,
);
if (!release || !Number.isSafeInteger(Number(release[3]) + 1))
  throw new Error(
    "Desktop package version must have an incrementable SemVer patch",
  );
export const appVersion = desktopPackage.version;
/** A later release, including when the current package is a prerelease. */
export const nextVersion = `${release[1]}.${release[2]}.${Number(release[3]) + 1}`;
/** A subscription without options of its own and without anything reported by its provider. */
export const noOptions = {
  interval_mins: null,
  user_agent: null,
  filters: { include: [], exclude: [], protocols: [] },
  provider_routes: true,
  usage: null,
  availability: "ok" as const,
};
export const fixture: Snapshot = {
  settings: {
    mode: "proxy",
    theme: "light",
    language: "ru",
    auto_select: true,
    proxy_port: 2080,
    api_port: 2081,
    interval_secs: 60,
    pool_size: 3,
    full_interval_mins: 10,
    degrade_percent: 50,
    degrade_cap_ms: 1500,
    endpoint_timeout_ms: 1500,
    proxy_timeout_ms: 5000,
    concurrency: 3,
    connectivity_url: "https://www.gstatic.com/generate_204",
    subscription_interval_mins: 60,
    switch_threshold_percent: 30,
    switch_threshold_ms: 50,
    autostart: false,
    auto_connect: false,
    find_process: false,
    geodata_auto_update: true,
  },
  servers: [
    {
      id: "a",
      name: "Frankfurt 01",
      protocol: "vless reality",
      source: "main",
      endpoint_ms: 31,
      proxy_ms: 64,
      status: "online",
      checked_at: 1789742400,
      failure_code: null,
    },
    {
      id: "b",
      name: "Helsinki 02",
      protocol: "vless",
      source: "main",
      endpoint_ms: 39,
      proxy_ms: null,
      status: "failed",
      checked_at: 1789742400,
      failure_code: null,
      retry_at: 1789742700,
    },
    {
      id: "c",
      name: "Amsterdam 01",
      protocol: "trojan",
      source: "backup",
      endpoint_ms: 48,
      proxy_ms: 81,
      status: "online",
      checked_at: 1789742400,
      failure_code: null,
    },
    {
      id: "d",
      name: "Warsaw 03",
      protocol: "shadowsocks",
      source: null,
      endpoint_ms: 52,
      proxy_ms: 74,
      status: "online",
      checked_at: 1789742400,
      failure_code: null,
    },
  ],
  subscriptions: [
    {
      id: "main",
      name: "Основная",
      count: 2,
      updated_at: 1789742400,
      error: null,
      failure_code: null,
      ...noOptions,
    },
    {
      id: "backup",
      name: "Резервная",
      count: 1,
      updated_at: 1789742400,
      error: null,
      failure_code: null,
      ...noOptions,
    },
  ],
  selected: "a",
  status: "connected",
  runtime_active: true,
  checking: false,
  refreshing: false,
  pending: false,
  next_check: null,
  error: null,
  welcome: false,
  servers_sort: { column: "name", descending: false },
  routing: {
    order: ["block", "direct", "proxy"],
    block: [],
    direct: [
      "domain:.ru",
      "domain:.xn--p1ai",
      "geosite:category-ru",
      "10.0.0.0/8",
      "172.16.0.0/12",
      "192.168.0.0/16",
      "169.254.0.0/16",
      "224.0.0.0/4",
      "255.255.255.255",
      "fc00::/7",
      "fe80::/10",
      "ff00::/8",
    ],
    proxy: [],
  },
  geodata: {
    geosite: null,
    geoip: null,
    updating: false,
    current: null,
    uses_geosite: true,
    uses_geoip: false,
    failure_code: null,
    available: true,
    wanted: true,
  },
  journal: [
    {
      at: 1789742400,
      level: "info",
      message: "Проверка серверов запущена",
      category: "checks",
    },
    {
      at: 1789742401,
      level: "info",
      message: "Рабочий сервер выбран",
      category: "checks",
    },
    {
      at: 1789742402,
      level: "info",
      message: "Подписка обновлена",
      category: "subscriptions",
    },
  ],
};
/** The part of a profile file that replaces the current data (the Rust core's `Store`). */
export type ProfileContents = Pick<
  Snapshot,
  | "settings"
  | "servers"
  | "subscriptions"
  | "selected"
  | "routing"
  | "servers_sort"
>;
type ProfileStep = "export" | "inspect" | "import";
interface ProfileFault {
  /** What the Rust host returns to React for this step (localized, no paths or content). */
  text: string;
  /** What the core also records as `Snapshot.error` (`Application::report_failure`). */
  core: string | null;
}
export class MockBackend implements BackendClient {
  private nextServerId = 0;
  private state: Snapshot;
  private listeners = new Set<(s: Snapshot) => void>();
  constructor(state: Snapshot = fixture) {
    this.state = structuredClone(state);
    const {
      settings,
      servers,
      subscriptions,
      selected,
      routing,
      servers_sort,
    } = this.state;
    this.profileFile = structuredClone({
      settings,
      servers,
      subscriptions,
      selected,
      routing,
      servers_sort,
    });
  }
  private newServerId() {
    let id: string;
    do {
      id = `added-${this.nextServerId++}`;
    } while (this.state.servers.some((server) => server.id === id));
    return id;
  }
  snapshot() {
    return Promise.resolve(structuredClone(this.state));
  }
  subscribe(listener: (s: Snapshot) => void) {
    this.listeners.add(listener);
    return Promise.resolve(() => {
      this.listeners.delete(listener);
    });
  }
  private emit() {
    for (const listener of this.listeners)
      listener(structuredClone(this.state));
  }
  /** Changes the state the way a backend event would and tells the subscribers. */
  update(change: (state: Snapshot) => void) {
    change(this.state);
    this.emit();
  }
  /** Every `watchConnections` call, in order. */
  watchingConnections: boolean[] = [];
  private connectionListeners = new Set<(list: Connections | null) => void>();
  /** What the core answers once the page starts watching (a story, a scenario). */
  connections: Connections | null = null;
  watchConnections(on: boolean) {
    this.watchingConnections.push(on);
    if (on && this.connections) {
      const list = this.connections;
      void Promise.resolve().then(() => this.pushConnections(list));
    }
    return Promise.resolve();
  }
  onConnections(listener: (list: Connections | null) => void) {
    this.connectionListeners.add(listener);
    return Promise.resolve(() => {
      this.connectionListeners.delete(listener);
    });
  }
  /** The core reports the connections (or `null`: nothing is being read). */
  pushConnections(list: Connections | null) {
    for (const listener of this.connectionListeners) listener(list);
  }
  /** Every `watchTraffic` call, in order. */
  watching: boolean[] = [];
  private trafficListeners = new Set<(speed: Speed | null) => void>();
  /** What the core answers once the screen starts watching (a story, a scenario). */
  speed: Speed | null = null;
  watchTraffic(on: boolean) {
    this.watching.push(on);
    if (on && this.speed) {
      const speed = this.speed;
      void Promise.resolve().then(() => this.pushTraffic(speed));
    }
    return Promise.resolve();
  }
  onTraffic(listener: (speed: Speed | null) => void) {
    this.trafficListeners.add(listener);
    return Promise.resolve(() => {
      this.trafficListeners.delete(listener);
    });
  }
  /** The core reports a speed (or `null`: nothing is being read). */
  pushTraffic(speed: Speed | null) {
    for (const listener of this.trafficListeners) listener(speed);
  }
  /** The measurements `history` answers with, by server id; a server without an entry has a
   * few made up from its row. */
  rows = new Map<string, HistoryRow[]>();
  historyAsked: string[] = [];
  history(id: string): Promise<HistoryRow[]> {
    this.historyAsked.push(id);
    const given = this.rows.get(id);
    if (given) return Promise.resolve(structuredClone(given));
    const server = this.state.servers.find((s) => s.id === id);
    if (!server?.checked_at) return Promise.resolve([]);
    return Promise.resolve(
      [2, 1, 0].map((back) => ({
        at: server.checked_at! - back * 60,
        endpoint_ms: server.endpoint_ms,
        proxy_ms: server.proxy_ms === null ? null : server.proxy_ms + back * 7,
        failure_code: server.proxy_ms === null ? server.failure_code : null,
      })),
    );
  }
  /** The addresses `subscriptionUrl` answers with, by subscription id. */
  subscriptionUrls: Record<string, string> = {
    main: "https://panel.example/main",
    backup: "https://panel.example/backup",
  };
  subscriptionUrl(id: string): Promise<string> {
    const url = this.subscriptionUrls[id];
    return url === undefined
      ? Promise.reject("Подписка не найдена")
      : Promise.resolve(url);
  }
  loggedErrors: string[] = [];
  loggedDetails: (string | undefined)[] = [];
  logError(message: string, detail?: string) {
    this.loggedErrors.push(message);
    this.loggedDetails.push(detail);
    return Promise.resolve();
  }
  private errorListeners = new Set<(message: string) => void>();
  onError(listener: (message: string) => void) {
    this.errorListeners.add(listener);
    return Promise.resolve(() => {
      this.errorListeners.delete(listener);
    });
  }
  private noticeListeners = new Set<(message: string) => void>();
  onNotice(listener: (message: string) => void) {
    this.noticeListeners.add(listener);
    return Promise.resolve(() => {
      this.noticeListeners.delete(listener);
    });
  }
  /** What the host does for every local information notice. */
  raiseNotice(message: string) {
    for (const listener of this.noticeListeners) listener(message);
  }
  /** What the host does for every local error notification. */
  raiseError(message: string) {
    for (const listener of this.errorListeners) listener(message);
  }
  profileExported = false;
  /** What the next "picked" profile file holds; `null` is a cancelled Open dialog. Applying
   * it replaces the profile data except machine-local autostart, as `import_profile` does.
   * By default it equals the starting state, so tests that need a visible change set it. */
  profileFile: ProfileContents | null;
  private staged: { request_id: number; contents: ProfileContents } | null =
    null;
  private nextRequestId = 0;
  private profileFaults = new Map<ProfileStep, ProfileFault>();
  private profileHolds = new Map<ProfileStep, Promise<void>>();
  /** The `Snapshot.error` text that a failed profile step recorded. A later successful profile
   * step clears the error only while it is still this one, so an error from elsewhere stays. */
  private profileError: string | null = null;
  /** Test/Storybook helper: the next `step` fails like the Rust host. React gets `text`;
   * a `core` message also becomes `Snapshot.error`, as the core records every profile failure. */
  failProfile(step: ProfileStep, text: string, core: string | null = null) {
    this.profileFaults.set(step, { text, core });
  }
  /** Test/Storybook helper: the next `step` does not answer (a native dialog or a write that
   * is still running) until the returned function is called. */
  holdProfile(step: ProfileStep) {
    let release = () => {};
    this.profileHolds.set(
      step,
      new Promise<void>((resolve) => {
        release = resolve;
      }),
    );
    return release;
  }
  private async waitProfile(step: ProfileStep) {
    const hold = this.profileHolds.get(step);
    this.profileHolds.delete(step);
    await hold;
  }
  private throwProfileFault(step: ProfileStep) {
    const fault = this.profileFaults.get(step);
    if (!fault) return;
    this.profileFaults.delete(step);
    if (fault.core !== null) {
      this.state.error = fault.core;
      this.profileError = fault.core;
      this.emit();
    }
    throw fault.text;
  }
  /** A successful profile step clears the error a profile step left, not a foreign one (a
   * sing-box crash, a selection cycle error); returns whether `Snapshot.error` changed. */
  private clearProfileError() {
    const cleared =
      this.profileError !== null && this.state.error === this.profileError;
    if (cleared) this.state.error = null;
    this.profileError = null;
    return cleared;
  }
  /** Test/Storybook helper: the core records an error that no profile step wrote (a sing-box
   * crash, a selection cycle failure); it replaces the current one and survives profile success. */
  recordCoreError(message: string) {
    this.state.error = message;
    this.profileError = null;
    this.emit();
  }
  async exportProfile() {
    await this.waitProfile("export");
    this.throwProfileFault("export");
    this.profileExported = true;
    if (this.clearProfileError()) this.emit();
    return true;
  }
  async inspectProfile() {
    // Every pick drops the previously checked file, as `ProfileImports::clear` does.
    this.staged = null;
    await this.waitProfile("inspect");
    if (!this.profileFile) return null;
    this.throwProfileFault("inspect");
    const contents = structuredClone(this.profileFile);
    const request_id = ++this.nextRequestId;
    this.staged = { request_id, contents };
    if (this.clearProfileError()) this.emit();
    return {
      request_id,
      version: 2,
      servers: contents.servers.length,
      manual_servers: contents.servers.filter((server) => !server.source)
        .length,
      subscriptions: contents.subscriptions.length,
      has_selected_server: contents.selected !== null,
    };
  }
  async importProfile(requestId: number) {
    // `ProfileImports::take`: the checked file is single use and taken before it is applied.
    const pending = this.staged;
    if (pending?.request_id !== requestId) {
      throw "Проверенный профиль больше недоступен; выберите файл снова";
    }
    this.staged = null;
    try {
      await this.waitProfile("import");
      this.throwProfileFault("import");
    } catch (error) {
      // A failed apply restores the request so the dialog can retry it.
      this.staged ??= pending;
      throw error;
    }
    const autostart = this.state.settings.autostart;
    Object.assign(this.state, structuredClone(pending.contents));
    this.state.settings.autostart = autostart;
    this.clearProfileError();
    this.emit();
    return this.snapshot();
  }
  appVersion() {
    return Promise.resolve(appVersion);
  }
  updateInfo: UpdateInfo | null = null;
  installed = false;
  private dismissedVersion: string | null = null;
  private updateListeners = new Set<(info: UpdateInfo) => void>();
  /** Test/Storybook helper: simulate a periodic or manual check finding a release. */
  pushUpdate(info: UpdateInfo | null) {
    this.updateInfo = info;
    if (info) for (const l of this.updateListeners) l(info);
  }
  checkForUpdate() {
    // Mirrors the Rust side: a dismissed version stays dismissed on a manual recheck too.
    if (this.updateInfo?.version === this.dismissedVersion) {
      return Promise.resolve(null);
    }
    return Promise.resolve(this.updateInfo);
  }
  installUpdate() {
    this.installed = true;
    return Promise.resolve();
  }
  dismissUpdate(version: string) {
    this.dismissedVersion = version;
    return Promise.resolve();
  }
  onUpdate(listener: (info: UpdateInfo) => void) {
    this.updateListeners.add(listener);
    return Promise.resolve(() => {
      this.updateListeners.delete(listener);
    });
  }
  preview(uri: string): Promise<Server> {
    const scheme = /^(vless|vmess|trojan|ss):\/\//i
      .exec(uri)?.[1]
      ?.toLowerCase();
    if (!scheme) return Promise.reject("Некорректный URI сервера");
    return Promise.resolve({
      ...fixture.servers[0],
      id: "new-server",
      name: "Новый сервер",
      protocol: scheme === "ss" ? "shadowsocks" : scheme,
      source: null,
      endpoint_ms: null,
      proxy_ms: null,
      status: "unchecked",
      checked_at: null,
      retry_at: null,
      failure_code: null,
      excluded: false,
    });
  }
  /** What the next QR scan finds; `undefined` makes it fail like a picture without a code. */
  qr: InputPreview | null | undefined = undefined;
  private importListeners = new Set<(preview: InputPreview) => void>();
  scanQrFile(): Promise<InputPreview | null> {
    return this.qr === undefined
      ? Promise.reject("QR-код на изображении не найден")
      : Promise.resolve(this.qr);
  }
  scanQrClipboard(): Promise<InputPreview> {
    return this.qr
      ? Promise.resolve(this.qr)
      : Promise.reject("В буфере обмена нет изображения");
  }
  onImport(listener: (preview: InputPreview) => void) {
    this.importListeners.add(listener);
    return Promise.resolve(() => {
      this.importListeners.delete(listener);
    });
  }
  /** The `rift://` link that started Rift (it arrived before the page). */
  pendingImport: InputPreview | null = null;
  /** The program the next file dialog picks; `null`: the dialog is canceled. */
  application: string | null = null;
  pickApplication(): Promise<string | null> {
    return Promise.resolve(this.application);
  }
  takeImport(): Promise<InputPreview | null> {
    const waiting = this.pendingImport;
    this.pendingImport = null;
    return Promise.resolve(waiting);
  }
  /** A `rift://` link opened Rift. */
  pushImport(preview: InputPreview) {
    for (const listener of this.importListeners) listener(preview);
  }
  previewInput(text: string): Promise<InputPreview> {
    const links = [...text.matchAll(/(?:vless|vmess|trojan|ss):\/\/\S+/gi)].map(
      (match) => match[0],
    );
    if (links.length > 0) {
      return Promise.resolve({
        kind: "servers",
        rejected: 0,
        items: links.map((uri, index) => ({
          uri,
          server: {
            id: `pasted-${index}`,
            name: `Новый сервер ${index + 1}`,
            protocol: /^ss:\/\//i.test(uri)
              ? "shadowsocks"
              : uri.slice(0, uri.indexOf(":")).toLowerCase(),
            source: null,
            endpoint_ms: null,
            proxy_ms: null,
            status: "unchecked",
            checked_at: null,
            retry_at: null,
            failure_code: null,
            excluded: false,
          },
        })),
      });
    }
    // Rust drops the punctuation that ends a sentence; so does this double.
    const url = /https:\/\/\S+/i.exec(text)?.[0].replace(/[.,;:!?)\]}>]+$/, "");
    if (url) return Promise.resolve({ kind: "subscription", url });
    return Promise.reject("В тексте нет ссылки подписки или сервера");
  }
  async execute(action: Action) {
    switch (action.type) {
      case "connect":
        this.state.status = "connected";
        this.state.runtime_active = true;
        break;
      case "disconnect":
        this.state.status = "disconnected";
        this.state.runtime_active = false;
        break;
      case "set_auto":
        this.state.settings.auto_select = action.enabled;
        break;
      case "select":
        // A choice by hand leaves Auto Select as it was: the server is compared like any other.
        this.state.selected = action.id;
        break;
      case "check_now":
        this.state.pending = true;
        break;
      case "save_settings":
        this.state.settings = action.settings;
        break;
      case "clear_journal":
        this.state.journal = [];
        break;
      case "add_server":
        this.state.servers.push({
          ...(await this.preview(action.uri)),
          id: this.newServerId(),
        });
        break;
      case "add_servers": {
        const staged = await Promise.all(
          action.uris.map((uri) => this.preview(uri)),
        );
        this.state.servers.push(
          ...staged.map((added) => ({
            ...added,
            id: this.newServerId(),
          })),
        );
        this.state.welcome = false;
        break;
      }
      case "set_server_excluded":
        this.state.servers = this.state.servers.map((s) =>
          s.id === action.id ? { ...s, excluded: action.excluded } : s,
        );
        break;
      case "remove_server":
        this.state.servers = this.state.servers.filter(
          (s) => s.id !== action.id,
        );
        break;
      case "save_subscription": {
        const old = this.state.subscriptions.find((s) => s.id === action.id);
        const item = {
          id: action.id ?? "new-sub",
          // The provider names it; until the first refresh the host of the URL does.
          name: old?.name ?? new URL(action.url).hostname,
          count: old?.count ?? 0,
          updated_at: old?.updated_at ?? null,
          error: null,
          failure_code: null,
          interval_mins: action.interval_mins,
          user_agent: action.user_agent,
          filters: action.filters,
          provider_routes: action.provider_routes,
          usage: old?.usage ?? null,
          availability: old?.availability ?? ("ok" as const),
        };
        this.state.subscriptions = this.state.subscriptions
          .filter((s) => s.id !== item.id)
          .concat(item);
        this.state.welcome = false;
        break;
      }
      case "save_routing": {
        const entries = (text: string) =>
          text
            .split(/[\n,;]/)
            .map((entry) => entry.trim())
            .filter(Boolean);
        for (const entry of [
          ...entries(action.block),
          ...entries(action.direct),
          ...entries(action.proxy),
        ]) {
          if (entry === "not a rule")
            throw "Список Block: Не удалось разобрать «not a rule»";
        }
        this.state.routing = {
          order: action.order,
          block: entries(action.block),
          direct: entries(action.direct),
          proxy: entries(action.proxy),
        };
        break;
      }
      case "reset_routing":
        this.state.routing = structuredClone(fixture.routing);
        break;
      case "update_geodata":
        this.state.geodata = {
          ...this.state.geodata,
          geosite: {
            updated_at: 1789742400,
            bytes: 11_205_406,
            checked_at: 1789742400,
          },
          geoip: {
            updated_at: 1789742400,
            bytes: 16_587_403,
            checked_at: 1789742400,
          },
          failure_code: null,
        };
        break;
      case "dismiss_welcome":
        this.state.welcome = false;
        break;
      case "set_servers_sort":
        this.state.servers_sort = {
          column: action.column,
          descending: action.descending,
        };
        break;
      case "remove_subscription":
        this.state.subscriptions = this.state.subscriptions.filter(
          (s) => s.id !== action.id,
        );
        break;
      case "refresh":
      case "export_diagnostics":
        break;
    }
    for (const l of this.listeners) l(structuredClone(this.state));
    return this.snapshot();
  }
}
