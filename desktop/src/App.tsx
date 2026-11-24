import { nextSort, visibleServers } from "./viewmodel";
import { useEffect, useRef, useState } from "react";
import {
  Plus,
  RefreshCw,
  Download,
  Upload,
  ArrowUpRight,
  QrCode,
  ArrowDown,
  ArrowUp,
} from "lucide-react";
import { useAppState, useBackend } from "./backend/provider";
import { useUpdateInfo, type UpdateController } from "./backend/update";
import type {
  Action,
  InputPreview,
  JournalCategory,
  ProfilePreview,
  Settings,
  Snapshot,
  Speed,
  Subscription,
} from "./backend/types";
import { UpdateBanner } from "./components/update";
import { ErrorToasts } from "./components/errors";
import { shownText, useErrorToasts } from "./backend/errors";
import {
  Alert,
  Badge,
  Button,
  ConnectionStatus,
  ConfirmationDialog,
  EmptyState,
  Field,
  LatencyDisplay,
  LinkStatus,
  LoadingState,
  ProxyAddress,
  RiftMark,
  Segmented,
  Select,
  Toggle,
  Toolbar,
} from "./components/ui";
import { RoutingScreen } from "./components/routing";
import { ServerTable, SubscriptionItem } from "./components/servers";
import { ConnectionHistory, HistoryPanel } from "./components/history";
import { ConnectionsScreen } from "./components/connections";
import {
  AddServerDialog,
  SubscriptionDialog,
  WelcomeDialog,
} from "./components/dialogs";
import { countdown, dateTime, modeLabel, speedText } from "./format";
import { emptyServers } from "./emptyServers";
import { useSettingsDraft } from "./useSettingsDraft";
import { LANGUAGES, t, tFor } from "./i18n";
export type Page =
  "connection" | "servers" | "connections" | "routing" | "settings" | "journal";
const pages = [
  "connection",
  "servers",
  "connections",
  "routing",
  "settings",
  "journal",
] as const;
export function Navigation({
  page,
  onChange,
}: {
  page: Page;
  onChange: (p: Page) => void;
}) {
  return (
    <nav aria-label={t("nav.aria")}>
      {pages.map((id) => (
        <button
          key={id}
          aria-current={page === id ? "page" : undefined}
          onClick={() => onChange(id)}
        >
          {t(`nav.${id}`)}
        </button>
      ))}
    </nav>
  );
}
/**
 * Ctrl+V on the page of servers: a link in the clipboard text opens the right dialog with it.
 * The text goes to Rust, which decides what it is; text that holds no link does nothing, and a
 * paste into a field is the field's own.
 */
function usePasteImport(
  enabled: boolean,
  open: (preview: InputPreview) => void,
) {
  const api = useBackend();
  const opener = useRef(open);
  useEffect(() => {
    opener.current = open;
  });
  useEffect(() => {
    if (!enabled) return;
    const onPaste = (event: ClipboardEvent) => {
      const target = event.target;
      if (
        target instanceof Element &&
        target.closest("input, textarea, select, [contenteditable]")
      )
        return;
      const text = event.clipboardData?.getData("text") ?? "";
      if (!text.trim()) return;
      event.preventDefault();
      void api
        .previewInput?.(text)
        .then((preview) => opener.current(preview))
        .catch(() => {});
    };
    document.addEventListener("paste", onPaste);
    return () => document.removeEventListener("paste", onPaste);
  }, [api, enabled]);
}
export function App({ initialPage = "connection" }: { initialPage?: Page }) {
  const { state, error, busy, execute } = useAppState();
  const api = useBackend();
  // Unknown until the host answers: the interface carries no release number of its own.
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    void api
      .appVersion?.()
      .then(setVersion)
      .catch(() => {});
  }, [api]);
  const update = useUpdateInfo();
  const [page, setPage] = useState<Page>(initialPage);
  // What the banner already says is not shown a second time as a toast.
  const bannerText = error ?? state?.error;
  const errors = useErrorToasts(bannerText);
  const [dialog, setDialog] = useState<"server" | "subscription" | null>(null);
  const [editing, setEditing] = useState<Subscription | undefined>();
  // What a paste, a QR code or a deep link brought: shown in a dialog, added only on confirm.
  const [incoming, setIncoming] = useState<InputPreview | null>(null);
  const closeDialog = () => {
    setDialog(null);
    setIncoming(null);
  };
  const openIncoming = (preview: InputPreview) => {
    setPage("servers");
    setEditing(undefined);
    setIncoming(preview);
    setDialog(preview.kind === "subscription" ? "subscription" : "server");
  };
  usePasteImport(
    page === "servers" && dialog === null && !state?.welcome,
    openIncoming,
  );
  // A `rift://` link opened Rift (or raised it): the same dialog as for a paste.
  const incomingOpener = useRef(openIncoming);
  useEffect(() => {
    incomingOpener.current = openIncoming;
  });
  useEffect(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    void (async () => {
      const off = await api.onImport?.((preview) =>
        incomingOpener.current(preview),
      );
      if (gone) off?.();
      else stop = off;
      // The link that started Rift arrived before this page; from here on links come as events.
      const waiting = await api.takeImport?.();
      if (waiting && !gone) incomingOpener.current(waiting);
    })();
    return () => {
      gone = true;
      stop?.();
    };
  }, [api]);
  // A QR code from a picture: failures are shown by the usual error path (the host reports them).
  const scanQr = (source: "file" | "clipboard") => {
    void (source === "file" ? api.scanQrFile?.() : api.scanQrClipboard?.())
      ?.then((preview) => {
        if (preview) openIncoming(preview);
      })
      .catch(() => {});
  };
  const [confirm, setConfirm] = useState<{
    title: string;
    action: Action;
  } | null>(null);
  const theme = state?.settings.theme;
  useEffect(() => {
    if (!theme) return;
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.theme =
        theme === "system" ? (media.matches ? "dark" : "light") : theme;
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
  const run = (a: Action) => {
    void execute(a);
  };
  // What the banner already says is not shown a second time as a toast.
  const banner = error ?? state?.error;
  const toasts = errors.toasts.filter(
    (toast) => !banner || toast.text !== shownText(banner),
  );
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <RiftMark className="brand-mark" />
          {t("app.name")}
        </div>
        <Navigation page={page} onChange={setPage} />
        {state && page !== "connection" && <LinkStatus status={state.status} />}
      </header>
      <main>
        {(error || state?.error) && (
          <div className="notice">
            <Alert>{error ?? state?.error}</Alert>
            {state?.status === "error" && (
              <div className="recovery-actions">
                {state.servers.length > 0 && (
                  <Button
                    variant="primary"
                    loading={busy}
                    onClick={() =>
                      run({
                        type: state.runtime_active ? "check_now" : "connect",
                      })
                    }
                  >
                    {state.runtime_active
                      ? t("connection_page.retry_check")
                      : t("connection_page.retry_connect")}
                  </Button>
                )}
                <Button onClick={() => setPage("servers")}>
                  {t("connection_page.servers_and_subscriptions")}
                </Button>
                <Button variant="ghost" onClick={() => setPage("journal")}>
                  {t("connection_page.open_journal")}
                </Button>
              </div>
            )}
          </div>
        )}
        {!state ? (
          <LoadingState label={t("connection_page.loading")} />
        ) : (
          <>
            {page === "connection" && (
              <ConnectionScreen
                state={state}
                busy={busy}
                execute={execute}
                openServers={() => setPage("servers")}
              />
            )}
            {page === "servers" && (
              <ServersScreen
                state={state}
                run={run}
                addServer={() => setDialog("server")}
                scanQr={scanQr}
                addSubscription={() => {
                  setEditing(undefined);
                  setDialog("subscription");
                }}
                editSubscription={(s) => {
                  setEditing(s);
                  setDialog("subscription");
                }}
                confirm={setConfirm}
              />
            )}
            {page === "connections" && <ConnectionsScreen state={state} />}
            {page === "routing" && (
              <RoutingScreen
                routing={state.routing}
                geodata={state.geodata}
                execute={execute}
              />
            )}
            {page === "settings" && (
              <SettingsScreen
                settings={state.settings}
                execute={execute}
                version={version}
                update={update}
              />
            )}
            {page === "journal" && <JournalScreen state={state} run={run} />}
          </>
        )}
      </main>
      <footer>
        <span className="version">
          {version ? t("app.name_version", { version }) : t("app.name")}
        </span>
      </footer>
      <ErrorToasts toasts={toasts} onDismiss={errors.dismiss} />
      {update.info && (
        <UpdateBanner
          info={update.info}
          onDismiss={() => void update.dismiss()}
          onInstall={update.install}
        />
      )}
      {state?.welcome && dialog === null && (
        <WelcomeDialog execute={execute} submitError={error} />
      )}
      {dialog === "server" && (
        <AddServerDialog
          incoming={incoming?.kind === "servers" ? incoming : undefined}
          onClose={closeDialog}
          execute={execute}
          submitError={error}
        />
      )}
      {dialog === "subscription" && (
        <SubscriptionDialog
          key={
            editing?.id ??
            (incoming?.kind === "subscription" ? incoming.url : "new")
          }
          item={editing}
          initialUrl={incoming?.kind === "subscription" ? incoming.url : ""}
          onClose={closeDialog}
          execute={execute}
          submitError={error}
        />
      )}
      {confirm && (
        <ConfirmationDialog
          title={confirm.title}
          message={t("connection_page.confirm_delete")}
          onClose={() => setConfirm(null)}
          onConfirm={() =>
            void execute(confirm.action).then((ok) => {
              if (ok) setConfirm(null);
            })
          }
        />
      )}
    </div>
  );
}
/** The speed through the proxy while this screen is open and Rift is connected; the core reads
 * sing-box only in that time. */
function useTraffic(connected: boolean): Speed | null {
  const api = useBackend();
  const [speed, setSpeed] = useState<Speed | null>(null);
  useEffect(() => {
    if (!connected || !api.watchTraffic || !api.onTraffic) {
      setSpeed(null);
      return;
    }
    let active = true;
    let dispose: (() => void) | undefined;
    const listener = (next: Speed | null) => {
      if (active) setSpeed(next);
    };
    void api.onTraffic(listener).then((fn) => {
      if (active) {
        dispose = fn;
        void api.watchTraffic?.(true);
      } else fn();
    });
    return () => {
      active = false;
      dispose?.();
      void api.watchTraffic?.(false);
      setSpeed(null);
    };
  }, [api, connected]);
  return speed;
}
export function ConnectionScreen({
  state,
  busy,
  execute,
  openServers,
}: {
  state: Snapshot;
  busy: boolean;
  execute: (a: Action) => Promise<boolean>;
  openServers: () => void;
}) {
  const [now, setNow] = useState(Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, []);
  const server = state.servers.find((s) => s.id === state.selected);
  const active = state.runtime_active;
  const mode = state.settings.mode;
  const speed = useTraffic(active);
  return (
    <section className="page" aria-label={t("nav.connection")}>
      <div className="connection-main">
        <div>
          <ConnectionStatus
            status={state.status}
            hasServers={state.servers.length > 0}
            hasSubscriptions={state.subscriptions.length > 0}
          />
          <Button
            variant="primary"
            className="connect-button"
            loading={busy && !active}
            disabled={state.servers.length === 0}
            onClick={() =>
              void execute({ type: active ? "disconnect" : "connect" })
            }
          >
            {active
              ? t("connection_page.disconnect")
              : t("connection_page.connect")}
          </Button>
          <div className="facts facts-secondary">
            <div className="fact">
              <div className="fact-label">{t("connection_page.mode")}</div>
              <strong>{modeLabel(mode)}</strong>
            </div>
            <div className="fact">
              <div className="fact-label">
                {t("connection_page.local_proxy")}
              </div>
              {mode === "tun" ? (
                <p className="muted">{t("connection_page.proxy_off_tun")}</p>
              ) : (
                <ProxyAddress port={state.settings.proxy_port} />
              )}
            </div>
          </div>
        </div>
        <div className="facts">
          <div className="fact">
            <div className="fact-label">{t("connection_page.server")}</div>
            <div className="fact-line">
              <div>
                <strong>
                  {server?.name ?? t("connection_page.no_server")}
                </strong>
                <p className="muted small">
                  {!server &&
                    (state.subscriptions.length > 0
                      ? t("connection_page.no_servers_subscription")
                      : t("connection_page.no_servers_add"))}
                </p>
              </div>
              <Button variant="ghost" onClick={openServers}>
                {t("connection_page.change")} <ArrowUpRight size={15} />
              </Button>
            </div>
          </div>
          <div className="fact">
            <div className="fact-label">{t("connection_page.speed")}</div>
            <div className="speeds">
              <span>
                <ArrowDown size={15} aria-hidden="true" />
                <span className="sr-only">{t("connection_page.download")}</span>
                <strong>{speed ? speedText(speed.down) : "—"}</strong>
              </span>
              <span>
                <ArrowUp size={15} aria-hidden="true" />
                <span className="sr-only">{t("connection_page.upload")}</span>
                <strong>{speed ? speedText(speed.up) : "—"}</strong>
              </span>
            </div>
          </div>
          <div className="fact">
            <Toggle
              label={t("connection_page.auto_select")}
              checked={state.settings.auto_select}
              onChange={(enabled) =>
                void execute({ type: "set_auto", enabled })
              }
            />
            <div className="check-row">
              <span>
                {state.checking
                  ? t("connection_page.choosing")
                  : state.pending
                    ? t("connection_page.pending")
                    : t("connection_page.next_check", {
                        time: countdown(state.next_check, now),
                      })}
              </span>
              <Button
                disabled={!active}
                onClick={() => void execute({ type: "check_now" })}
              >
                <RefreshCw size={14} /> {t("connection_page.check_now")}
              </Button>
            </div>
          </div>
          <details className="connection-details">
            <summary>{t("connection_page.details")}</summary>
            <div className="fact">
              <div className="fact-label">{t("connection_page.protocol")}</div>
              <strong>{server?.protocol.toUpperCase() ?? "—"}</strong>
            </div>
            <div className="fact">
              <LatencyDisplay
                endpoint={server?.endpoint_ms ?? null}
                proxy={server?.proxy_ms ?? null}
              />
            </div>
            <ConnectionHistory server={server} />
          </details>
        </div>
      </div>
    </section>
  );
}
function ServersScreen({
  state,
  run,
  addServer,
  scanQr,
  addSubscription,
  editSubscription,
  confirm,
}: {
  state: Snapshot;
  run: (a: Action) => void;
  addServer: () => void;
  scanQr: (source: "file" | "clipboard") => void;
  addSubscription: () => void;
  editSubscription: (s: Subscription) => void;
  confirm: (v: { title: string; action: Action }) => void;
}) {
  const [source, setSource] = useState("all");
  const [detailed, setDetailed] = useState(false);
  const servers = visibleServers(
    state.servers,
    source,
    state.subscriptions,
    state.servers_sort,
    state.selected,
  );
  const progress = state.checking
    ? t("servers_page.checking")
    : state.refreshing
      ? t("servers_page.refreshing")
      : "";
  return (
    <section className="page">
      <div className="page-head">
        <h1>{t("nav.servers")}</h1>
        <div className="page-actions">
          <span className="progress muted" role="status">
            {progress}
          </span>
        </div>
      </div>
      <div className="sources-layout">
        <aside className="sources">
          <div className="sources-head">
            <h2>{t("servers_page.subscriptions")}</h2>
          </div>
          {state.servers.length > 0 && (
            <Button variant="primary" onClick={addSubscription}>
              <Plus size={14} />
              {t("servers_page.add_subscription")}
            </Button>
          )}
          <Button
            variant="ghost"
            loading={state.refreshing}
            onClick={() => run({ type: "refresh", id: null })}
          >
            <RefreshCw size={14} />
            {t("servers_page.refresh_all")}
          </Button>
          <Button
            variant="ghost"
            disabled={!state.runtime_active}
            onClick={() => run({ type: "check_now" })}
          >
            <RefreshCw size={14} />
            {t("servers_page.check_all")}
          </Button>
          <button
            className={`source-filter source-all ${source === "all" ? "selected" : ""}`}
            onClick={() => setSource("all")}
          >
            {t("servers_page.all_servers")}{" "}
            <span className="source-count">{state.servers.length}</span>
          </button>
          {state.subscriptions.map((s) => (
            <SubscriptionItem
              key={s.id}
              item={s}
              selected={source === s.id}
              refreshing={state.refreshing}
              onSelect={() => setSource(s.id)}
              onRefresh={() => run({ type: "refresh", id: s.id })}
              onEdit={() => editSubscription(s)}
              onDelete={() =>
                confirm({
                  title: t("servers_page.delete_subscription", {
                    name: s.name,
                  }),
                  action: { type: "remove_subscription", id: s.id },
                })
              }
            />
          ))}
          {state.subscriptions.length === 0 && (
            <p className="muted small">{t("servers_page.no_subscriptions")}</p>
          )}
          <button
            className={`source-filter ${source === "manual" ? "selected" : ""}`}
            onClick={() => setSource("manual")}
          >
            {t("servers.manual")}{" "}
            <span className="source-count">
              {state.servers.filter((s) => !s.source).length}
            </span>
          </button>
        </aside>
        <section className="servers">
          <div className="servers-head">
            <Button
              variant="ghost"
              aria-expanded={detailed}
              onClick={() => setDetailed(!detailed)}
            >
              {detailed
                ? t("servers_page.hide_details")
                : t("servers_page.details")}
            </Button>
            <div className="servers-add">
              <Button variant="ghost" onClick={() => scanQr("file")}>
                <QrCode size={15} />
                {t("servers_page.qr_file")}
              </Button>
              <Button variant="ghost" onClick={() => scanQr("clipboard")}>
                <QrCode size={15} />
                {t("servers_page.qr_clipboard")}
              </Button>
              <Button onClick={addServer}>
                <Plus size={16} />
                {t("servers_page.add_server")}
              </Button>
            </div>
          </div>
          {servers.length ? (
            <ServerTable
              servers={servers}
              selected={state.selected}
              sources={state.subscriptions}
              sort={state.servers_sort}
              onSort={(column) =>
                run({
                  type: "set_servers_sort",
                  ...nextSort(state.servers_sort, column),
                })
              }
              detailed={detailed}
              onSelect={(id) => run({ type: "select", id })}
              onExclude={(id, excluded) =>
                run({ type: "set_server_excluded", id, excluded })
              }
              onDelete={(id) =>
                confirm({
                  title: t("servers_page.delete_server"),
                  action: { type: "remove_server", id },
                })
              }
            />
          ) : (
            <EmptyList
              state={state}
              source={source}
              run={run}
              addServer={addServer}
              addSubscription={addSubscription}
              editSubscription={editSubscription}
            />
          )}
        </section>
      </div>
    </section>
  );
}
/** The empty list of servers: why it is empty, and the one thing to do about it. */
function EmptyList({
  state,
  source,
  run,
  addServer,
  addSubscription,
  editSubscription,
}: {
  state: Snapshot;
  source: string;
  run: (a: Action) => void;
  addServer: () => void;
  addSubscription: () => void;
  editSubscription: (s: Subscription) => void;
}) {
  const empty = emptyServers(state, source);
  const { action } = empty;
  const act = () => {
    if (!action) return;
    switch (action.kind) {
      case "refresh":
        return run({ type: "refresh", id: action.id ?? null });
      case "refresh_all":
        return run({ type: "refresh", id: null });
      case "edit": {
        const subscription = state.subscriptions.find(
          (s) => s.id === action.id,
        );
        return subscription && editSubscription(subscription);
      }
      case "add_server":
        return addServer();
      case "add_subscription":
        return addSubscription();
    }
  };
  return (
    <EmptyState title={empty.title} description={empty.description}>
      {action && (
        <Button
          variant="primary"
          loading={
            state.refreshing &&
            (action.kind === "refresh" || action.kind === "refresh_all")
          }
          onClick={act}
        >
          {action.label}
        </Button>
      )}
    </EmptyState>
  );
}
export function SettingsScreen({
  settings,
  execute,
  version,
  update,
}: {
  settings: Settings;
  execute: (a: Action) => Promise<boolean>;
  version: string | null;
  update: UpdateController;
}) {
  const api = useBackend();
  // Every edit saves itself. Backend settings replace the draft only while nothing is waiting
  // to be saved (an imported profile, a finished save). The screen itself must not be
  // re-keyed: it owns the profile dialog, its result and the focus target, which a remount
  // would lose.
  const { draft, problems, choose, type, flush } = useSettingsDraft(
    settings,
    execute,
  );
  const [checking, setChecking] = useState(false);
  const [checked, setChecked] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [profileDialog, setProfileDialog] = useState<
    "export" | ProfilePreview | null
  >(null);
  const [profileBusy, setProfileBusy] = useState(false);
  // Both buttons are disabled while their operation runs, so the browser drops the focus
  // before the dialog opens; the dialog returns it here instead of to the page.
  const exportButton = useRef<HTMLButtonElement>(null);
  const importButton = useRef<HTMLButtonElement>(null);
  const [profileResult, setProfileResult] = useState<{
    text: string;
    error: boolean;
  } | null>(null);
  const profileError = (error: unknown, fallback: string) =>
    typeof error === "string" ? error : fallback;
  const chooseProfile = async () => {
    setProfileBusy(true);
    setProfileResult(null);
    try {
      const preview = await api.inspectProfile();
      if (preview) setProfileDialog(preview);
    } catch (error) {
      setProfileResult({
        text: profileError(error, t("profile.check_failed")),
        error: true,
      });
    } finally {
      setProfileBusy(false);
    }
  };
  const exportProfile = async () => {
    setProfileBusy(true);
    setProfileResult(null);
    try {
      const saved = await api.exportProfile();
      setProfileDialog(null);
      if (saved) {
        setProfileResult({ text: t("profile.exported"), error: false });
      }
    } catch (error) {
      setProfileResult({
        text: profileError(error, t("profile.export_failed")),
        error: true,
      });
    } finally {
      setProfileBusy(false);
    }
  };
  const importProfile = async (preview: ProfilePreview) => {
    setProfileBusy(true);
    setProfileResult(null);
    try {
      await api.importProfile(preview.request_id);
      setProfileDialog(null);
      setProfileResult({ text: t("profile.imported"), error: false });
    } catch (error) {
      setProfileResult({
        text: profileError(error, t("profile.import_failed")),
        error: true,
      });
    } finally {
      setProfileBusy(false);
    }
  };
  return (
    <div className="page page-form">
      <div className="page-head">
        <div>
          <h1>{t("nav.settings")}</h1>
          <p className="muted small">{t("settings.intro")}</p>
        </div>
      </div>
      <form
        id="settings-form"
        onSubmit={(e) => {
          e.preventDefault();
          flush();
        }}
      >
        <section className="group">
          <h2>{t("settings.general")}</h2>
          <Select
            label={t("settings.theme")}
            value={draft.theme}
            onChange={(e) =>
              choose("theme", e.target.value as Settings["theme"])
            }
          >
            <option value="system">{t("settings.theme_system")}</option>
            <option value="light">{t("settings.theme_light")}</option>
            <option value="dark">{t("settings.theme_dark")}</option>
          </Select>
          <Select
            label={t("settings.language")}
            value={draft.language}
            onChange={(e) =>
              choose("language", e.target.value as Settings["language"])
            }
          >
            {LANGUAGES.map((language) => (
              <option key={language.code} value={language.code}>
                {language.flag} {language.name}
              </option>
            ))}
          </Select>
          <Toggle
            label={t("settings.autostart.label")}
            checked={draft.autostart}
            onChange={(v) => choose("autostart", v)}
          />
          <p className="setting-hint">{t("settings.autostart.hint")}</p>
          <Toggle
            label={t("settings.auto_connect.label")}
            checked={draft.auto_connect}
            onChange={(v) => choose("auto_connect", v)}
          />
          <p className="setting-hint">{t("settings.auto_connect.hint")}</p>
          <Toggle
            label={t("settings.geodata.label")}
            checked={draft.geodata_auto_update}
            onChange={(v) => choose("geodata_auto_update", v)}
          />
          <p className="setting-hint">{t("settings.geodata.hint")}</p>
        </section>
        <section className="group">
          <h2>{t("settings.connection")}</h2>
          <Select
            label={t("settings.mode")}
            value={draft.mode}
            onChange={(e) => choose("mode", e.target.value as Settings["mode"])}
          >
            <option value="proxy">{modeLabel("proxy")}</option>
            <option value="tun">{modeLabel("tun")}</option>
            <option value="tun_proxy">{modeLabel("tun_proxy")}</option>
          </Select>
          <Alert tone="info">{t("settings.tun_notice")}</Alert>
          <div className="form-grid">
            <div className="static-field">
              <span>{t("settings.proxy_address")}</span>
              <code>127.0.0.1</code>
            </div>
            <Field
              label={t("settings.port")}
              type="number"
              min={1}
              max={65535}
              value={draft.proxy_port}
              error={problems.proxy_port}
              onChange={(e) => type("proxy_port", Number(e.target.value))}
              onBlur={flush}
            />
          </div>
          <Toggle
            label={t("settings.find_process.label")}
            checked={draft.find_process}
            onChange={(v) => choose("find_process", v)}
          />
          <p className="setting-hint">{t("settings.find_process.hint")}</p>
        </section>
        <section className="group">
          <h2>{t("settings.auto_select_title")}</h2>
          <Toggle
            label={t("settings.auto_select")}
            checked={draft.auto_select}
            onChange={(v) => choose("auto_select", v)}
          />
          <div className="form-grid">
            <Field
              label={t("settings.fast_check")}
              type="number"
              min={50}
              max={300}
              value={draft.interval_secs}
              error={problems.interval_secs}
              onChange={(e) => type("interval_secs", Number(e.target.value))}
              onBlur={flush}
            />
            <Field
              label={t("settings.full_check")}
              type="number"
              min={10}
              max={30}
              value={draft.full_interval_mins}
              error={problems.full_interval_mins}
              onChange={(e) =>
                type("full_interval_mins", Number(e.target.value))
              }
              onBlur={flush}
            />
            <Field
              label={t("settings.pool_size")}
              type="number"
              min={1}
              max={5}
              value={draft.pool_size}
              error={problems.pool_size}
              onChange={(e) => type("pool_size", Number(e.target.value))}
              onBlur={flush}
            />
          </div>
          <p className="muted small">{t("settings.interval_spread")}</p>
          <details>
            <summary>{t("common.advanced")}</summary>
            <div className="form-grid">
              <Field
                label={t("settings.degrade_percent")}
                type="number"
                min={10}
                max={300}
                value={draft.degrade_percent}
                error={problems.degrade_percent}
                onChange={(e) =>
                  type("degrade_percent", Number(e.target.value))
                }
                onBlur={flush}
              />
              <Field
                label={t("settings.degrade_cap")}
                type="number"
                min={200}
                max={10000}
                value={draft.degrade_cap_ms}
                error={problems.degrade_cap_ms}
                onChange={(e) => type("degrade_cap_ms", Number(e.target.value))}
                onBlur={flush}
              />
            </div>
            <p className="muted small">{t("settings.degrade_note")}</p>
            <div className="form-grid">
              <Field
                label={t("settings.switch_percent")}
                type="number"
                min={0}
                max={100}
                value={draft.switch_threshold_percent}
                error={problems.switch_threshold_percent}
                onChange={(e) =>
                  type("switch_threshold_percent", Number(e.target.value))
                }
                onBlur={flush}
              />
              <Field
                label={t("settings.switch_ms")}
                type="number"
                min={0}
                max={10000}
                value={draft.switch_threshold_ms}
                error={problems.switch_threshold_ms}
                onChange={(e) =>
                  type("switch_threshold_ms", Number(e.target.value))
                }
                onBlur={flush}
              />
            </div>
            <p className="muted small">{t("settings.switch_note")}</p>
          </details>
        </section>
        <section className="group">
          <h2>{t("settings.subscriptions")}</h2>
          <div className="form-grid">
            <Field
              label={t("settings.subscription_interval")}
              type="number"
              min={1}
              max={10080}
              value={draft.subscription_interval_mins}
              error={problems.subscription_interval_mins}
              onChange={(e) =>
                type("subscription_interval_mins", Number(e.target.value))
              }
              onBlur={flush}
            />
          </div>
          <p className="muted small">{t("settings.interval_spread")}</p>
          <details>
            <summary>{t("common.advanced")}</summary>
            <div className="form-grid">
              {(
                [
                  [
                    "endpoint_timeout_ms",
                    "settings.endpoint_timeout",
                    100,
                    30000,
                  ],
                  ["proxy_timeout_ms", "settings.proxy_timeout", 100, 30000],
                  ["concurrency", "settings.concurrency", 1, 3],
                ] as const
              ).map(([key, label, min, max]) => (
                <Field
                  key={key}
                  label={t(label)}
                  type="number"
                  min={min}
                  max={max}
                  value={draft[key]}
                  error={problems[key]}
                  onChange={(e) => type(key, Number(e.target.value))}
                  onBlur={flush}
                />
              ))}
              <Field
                label={t("settings.api_port")}
                type="number"
                min={1}
                max={65535}
                value={draft.api_port}
                error={problems.api_port}
                onChange={(e) => type("api_port", Number(e.target.value))}
                onBlur={flush}
              />
            </div>
            <Field
              label={t("settings.connectivity_url")}
              type="url"
              value={draft.connectivity_url}
              error={problems.connectivity_url}
              onChange={(e) => type("connectivity_url", e.target.value)}
              onBlur={flush}
            />
          </details>
        </section>
        <section className="group">
          <h2>{t("settings.profile")}</h2>
          <p className="muted">{t("settings.profile_note")}</p>
          <div className="profile-actions">
            <Button
              ref={exportButton}
              type="button"
              onClick={() => {
                setProfileResult(null);
                setProfileDialog("export");
              }}
              disabled={profileBusy}
            >
              <Download size={16} />
              {t("settings.export")}
            </Button>
            <Button
              ref={importButton}
              type="button"
              onClick={() => void chooseProfile()}
              loading={profileBusy && profileDialog === null}
              disabled={profileBusy}
            >
              <Upload size={16} />
              {t("settings.import")}
            </Button>
          </div>
          <p className="muted small">{t("settings.profile_secrets")}</p>
          {profileResult && profileDialog === null && (
            <Alert tone={profileResult.error ? "error" : "info"}>
              {profileResult.text}
            </Alert>
          )}
        </section>
        <section className="group">
          <h2>{t("settings.about")}</h2>
          <p className="muted">
            {t("settings.version", {
              version: version ?? t("settings.version_unknown"),
            })}
          </p>
          <div className="profile-actions">
            <Button
              type="button"
              onClick={() => {
                setChecking(true);
                setChecked(false);
                setCheckError(null);
                update
                  .check()
                  .then(() => setChecked(true))
                  .catch((e: unknown) =>
                    setCheckError(
                      typeof e === "string"
                        ? e
                        : t("settings.update_check_failed"),
                    ),
                  )
                  .finally(() => setChecking(false));
              }}
              loading={checking}
            >
              {t("settings.check_updates")}
            </Button>
          </div>
          {checkError && <Alert>{checkError}</Alert>}
          {checked && !checking && !checkError && !update.info && (
            <Alert tone="info">{t("settings.up_to_date")}</Alert>
          )}
          {update.info && (
            <Alert tone="info">
              {t("update.available", { version: update.info.version })}
            </Alert>
          )}
        </section>
      </form>
      {profileDialog === "export" && (
        <ConfirmationDialog
          title={t("profile.export_title")}
          message={
            <>
              <Alert tone="error">{t("profile.export_warning")}</Alert>
              {profileResult?.error && <Alert>{profileResult.text}</Alert>}
            </>
          }
          confirmLabel={t("profile.export_confirm")}
          confirmVariant="primary"
          loading={profileBusy}
          returnFocus={exportButton}
          onClose={() => setProfileDialog(null)}
          onConfirm={() => void exportProfile()}
        />
      )}
      {profileDialog !== null && profileDialog !== "export" && (
        <ConfirmationDialog
          title={t("profile.import_title")}
          message={
            <div className="profile-preview">
              <p>
                {t("profile.checked_version", {
                  version: profileDialog.version,
                })}
              </p>
              <dl>
                <div>
                  <dt>{t("profile.servers")}</dt>
                  <dd>{profileDialog.servers}</dd>
                </div>
                <div>
                  <dt>{t("profile.manual_servers")}</dt>
                  <dd>{profileDialog.manual_servers}</dd>
                </div>
                <div>
                  <dt>{t("profile.subscriptions")}</dt>
                  <dd>{profileDialog.subscriptions}</dd>
                </div>
                <div>
                  <dt>{t("profile.selected_server")}</dt>
                  <dd>
                    {profileDialog.has_selected_server
                      ? t("profile.present")
                      : t("profile.absent")}
                  </dd>
                </div>
              </dl>
              <Alert tone="error">{t("profile.replace_warning")}</Alert>
              {profileResult?.error && <Alert>{profileResult.text}</Alert>}
            </div>
          }
          confirmLabel={t("profile.import_confirm")}
          confirmVariant="primary"
          loading={profileBusy}
          returnFocus={importButton}
          onClose={() => setProfileDialog(null)}
          onConfirm={() => void importProfile(profileDialog)}
        />
      )}
    </div>
  );
}
/** The sources of the journal, as tabs: `all` is not a source, the rest are. */
const JOURNAL_TABS: ("all" | JournalCategory)[] = [
  "all",
  "app",
  "subscriptions",
  "checks",
  "core",
];
export function JournalScreen({
  state,
  run,
}: {
  state: Snapshot;
  run: (a: Action) => void;
}) {
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState("all");
  const [source, setSource] = useState<"all" | JournalCategory>("all");
  const [exportOpen, setExportOpen] = useState(false);
  const exportButton = useRef<HTMLButtonElement>(null);
  // Identity comes from the record, not its filtered/rotated table position.
  // The occurrence suffix keeps byte-identical same-second records as separate rows.
  const occurrences = new Map<string, number>();
  const entries = state.journal
    .map((entry) => {
      const identity =
        entry.crash_id ??
        JSON.stringify([
          entry.at,
          entry.category,
          entry.level,
          entry.message,
          entry.details,
          entry.failure_code,
        ]);
      const occurrence = occurrences.get(identity) ?? 0;
      occurrences.set(identity, occurrence + 1);
      return { entry, key: `${identity}:${occurrence}` };
    })
    .filter(
      ({ entry: e }) =>
        (source === "all" || e.category === source) &&
        (level === "all" || e.level === level) &&
        `${e.message}\n${e.details ?? ""}`
          .toLocaleLowerCase()
          .includes(search.toLocaleLowerCase()),
    );
  const sourceLabel = (category: JournalCategory) =>
    tFor("journal.tab", category) ?? t("journal.tab.app");
  return (
    <div className="page">
      <div className="page-head">
        <h1>{t("journal.title")}</h1>
        <Button ref={exportButton} onClick={() => setExportOpen(true)}>
          <Download size={15} />
          {t("journal.export_diagnostics")}
        </Button>
      </div>
      <Segmented
        label={t("journal.source")}
        value={source}
        options={JOURNAL_TABS.map((value) => ({
          value,
          label: t(`journal.tab.${value}`),
        }))}
        onChange={(value) => setSource(value as "all" | JournalCategory)}
      />
      {source === "checks" && (
        <HistoryPanel servers={state.servers} selected={state.selected} />
      )}
      <Toolbar>
        <Field
          label={t("journal.search")}
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <Select
          label={t("journal.level")}
          value={level}
          onChange={(e) => setLevel(e.target.value)}
        >
          <option value="all">{t("journal.level_all")}</option>
          <option value="info">{t("journal.level_info")}</option>
          <option value="warn">{t("journal.level_warn")}</option>
          <option value="error">{t("journal.level_error")}</option>
        </Select>
        <Button onClick={() => run({ type: "clear_journal" })}>
          {t("journal.clear")}
        </Button>
      </Toolbar>
      {entries.length ? (
        <div
          className="table-scroll"
          role="region"
          aria-label={t("journal.records")}
          tabIndex={0}
        >
          <table>
            <thead>
              <tr>
                <th>{t("journal.col_time")}</th>
                <th className="journal-level">{t("journal.col_level")}</th>
                {source === "all" && <th>{t("journal.col_source")}</th>}
                <th>{t("journal.col_message")}</th>
              </tr>
            </thead>
            <tbody>
              {entries.map(({ entry: e, key }) => (
                <tr key={key}>
                  <td>{dateTime(e.at)}</td>
                  <td className="journal-level">
                    {e.level === "error" ? (
                      <Badge tone="error">{t("journal.badge_error")}</Badge>
                    ) : e.level === "warn" ? (
                      <Badge tone="info">{t("journal.badge_warn")}</Badge>
                    ) : (
                      <span className="muted">{t("journal.badge_info")}</span>
                    )}
                  </td>
                  {source === "all" && (
                    <td className="muted">{sourceLabel(e.category)}</td>
                  )}
                  <td>
                    {e.message}
                    {e.details && (
                      <details className="journal-details">
                        <summary>{t("journal.details")}</summary>
                        <pre>{e.details}</pre>
                      </details>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : state.journal.length > 0 ? (
        <EmptyState
          title={t("journal.nothing_found_title")}
          description={t("journal.nothing_found_description")}
        />
      ) : (
        <EmptyState
          title={t("journal.quiet_title")}
          description={t("journal.quiet_description")}
        />
      )}
      {exportOpen && (
        <ConfirmationDialog
          title={t("journal.export_title")}
          message={<Alert tone="info">{t("journal.export_warning")}</Alert>}
          confirmLabel={t("journal.export_confirm")}
          confirmVariant="primary"
          returnFocus={exportButton}
          onClose={() => setExportOpen(false)}
          onConfirm={() => {
            setExportOpen(false);
            run({ type: "export_diagnostics" });
          }}
        />
      )}
    </div>
  );
}
