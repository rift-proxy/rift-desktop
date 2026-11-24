import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ipcMetrics, measured, measuredListener } from "./metrics";
import type {
  BackendClient,
  Connections,
  HistoryRow,
  InputPreview,
  ProfilePreview,
  Snapshot,
  Server,
  Speed,
  UpdateInfo,
} from "./types";
declare global {
  // DevTools access to IPC timings.
  var riftPerf: {
    ipc: () => ReturnType<typeof ipcMetrics.report>;
    reset: () => void;
  };
}
globalThis.riftPerf = {
  ipc: () => ipcMetrics.report(),
  reset: () => ipcMetrics.reset(),
};
export const tauriBackend: BackendClient = {
  snapshot: () => measured("snapshot", () => invoke<Snapshot>("snapshot")),
  execute: (action) =>
    measured(`execute:${action.type}`, () =>
      invoke<Snapshot>("execute", { action }),
    ),
  preview: (uri) =>
    measured("preview_server", () => invoke<Server>("preview_server", { uri })),
  previewInput: (text) =>
    measured("preview_input", () =>
      invoke<InputPreview>("preview_input", { text }),
    ),
  scanQrFile: () =>
    measured("scan_qr_file", () => invoke<InputPreview | null>("scan_qr_file")),
  scanQrClipboard: () =>
    measured("scan_qr_clipboard", () =>
      invoke<InputPreview>("scan_qr_clipboard"),
    ),
  pickApplication: () =>
    measured("pick_application", () =>
      invoke<string | null>("pick_application"),
    ),
  history: (id) =>
    measured("server_history", () =>
      invoke<HistoryRow[]>("server_history", { id }),
    ),
  subscriptionUrl: (id) =>
    measured("subscription_url", () =>
      invoke<string>("subscription_url", { id }),
    ),
  watchTraffic: (on) => invoke<void>("watch_traffic", { on }),
  onTraffic: (listener) =>
    listen<Speed | null>(
      "rift://traffic",
      measuredListener("event:rift://traffic", (event) =>
        listener(event.payload),
      ),
    ),
  watchConnections: (on) => invoke<void>("watch_connections", { on }),
  onConnections: (listener) =>
    listen<Connections | null>(
      "rift://connections",
      measuredListener("event:rift://connections", (event) =>
        listener(event.payload),
      ),
    ),
  takeImport: () => invoke<InputPreview | null>("take_import"),
  onImport: (listener) =>
    listen<InputPreview>(
      "rift://import",
      measuredListener("event:rift://import", (event) =>
        listener(event.payload),
      ),
    ),
  subscribe: (listener) =>
    listen<Snapshot>(
      "rift://state",
      measuredListener("event:rift://state", (event) =>
        listener(event.payload),
      ),
    ),
  logError: (message, detail) =>
    invoke<void>("log_frontend_error", { message, detail }),
  exportProfile: () =>
    measured("export_profile", () => invoke<boolean>("export_profile")),
  inspectProfile: () =>
    measured("inspect_profile", () =>
      invoke<ProfilePreview | null>("inspect_profile"),
    ),
  importProfile: (requestId) =>
    measured("import_profile", () =>
      invoke<Snapshot>("import_profile", { requestId }),
    ),
  ready: () => invoke<void>("ui_ready"),
  appVersion: () =>
    measured("app_version", () => invoke<string>("app_version")),
  checkForUpdate: () =>
    measured("check_for_update", () =>
      invoke<UpdateInfo | null>("check_for_update"),
    ),
  installUpdate: () =>
    measured("install_update", () => invoke<void>("install_update")),
  dismissUpdate: (version) => invoke<void>("dismiss_update", { version }),
  onUpdate: (listener) =>
    listen<UpdateInfo>(
      "rift://update",
      measuredListener("event:rift://update", (event) =>
        listener(event.payload),
      ),
    ),
  onNotice: (listener) =>
    listen<string>(
      "rift://notice",
      measuredListener("event:rift://notice", (event) =>
        listener(event.payload),
      ),
    ),
  onError: (listener) =>
    listen<string>(
      "rift://error",
      measuredListener("event:rift://error", (event) =>
        listener(event.payload),
      ),
    ),
};
