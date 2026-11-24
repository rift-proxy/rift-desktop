import { useMemo } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";
import { App, type Page } from "./App";
import { BackendProvider } from "./backend/provider";
import type {
  ConnectionRow,
  Connections as ConnectionList,
  Server,
  InputPreview,
  Snapshot,
} from "./backend/types";
import { MockBackend, fixture, nextVersion } from "./testing/mock";
import coreContract from "./testing/core-contract.json";
function Screen({
  state = fixture,
  page = "connection",
  prepare,
}: {
  state?: Snapshot;
  page?: Page;
  /** Scenario setup for the mock backend (a picked profile file, a held or failing step). */
  prepare?: (backend: MockBackend) => void;
}) {
  const client = useMemo(() => {
    const backend = new MockBackend(state);
    prepare?.(backend);
    return backend;
  }, [state, prepare]);
  return (
    <BackendProvider client={client}>
      <App initialPage={page} />
    </BackendProvider>
  );
}
const meta = { title: "Rift/Экраны", component: Screen } satisfies Meta<
  typeof Screen
>;
export default meta;
type Story = StoryObj<typeof meta>;
export const Connected: Story = {
  args: { state: fixture },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(
      await c.findByRole("heading", { name: "Подключено" }),
    ).toBeVisible();
  },
};
export const Disconnected: Story = {
  args: {
    state: { ...fixture, status: "disconnected", runtime_active: false },
  },
};
export const Connecting: Story = {
  args: { state: { ...fixture, status: "connecting", checking: true } },
};
export const Error: Story = {
  args: {
    state: { ...fixture, status: "error", error: "Нет доступных серверов" },
  },
};
export const ConnectionLost: Story = {
  args: {
    state: {
      ...fixture,
      status: "lost",
      settings: { ...fixture.settings, auto_select: false },
    },
  },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByRole("heading", {
        name: "Соединение потеряно",
      }),
    ).toBeVisible();
  },
};
export const ConnectionSpeed: Story = {
  args: {
    prepare: (backend: MockBackend) => {
      backend.speed = { up: 312 * 1024, down: 4.6 * 1024 ** 2 };
    },
  },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText("4,6 МБ/с"),
    ).toBeVisible();
  },
};
export const Dark: Story = {
  args: {
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};
export const Servers: Story = { args: { page: "servers" } };
/** One server the user keeps out of Auto Select: grey, marked, and after all the others. */
export const ServersKeptOut: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      servers: fixture.servers.map((s) =>
        s.id === "a" ? { ...s, excluded: true } : s,
      ),
    },
  },
  play: async ({ canvasElement }) => {
    const mark = await within(canvasElement).findByText("Исключён");
    await expect(mark).toBeVisible();
    // The mark is set off from the name; the status chip of the row is not moved by it, and its
    // left edge is the one of the chips of the other rows.
    const row = mark.closest("tr")!;
    await expect(getComputedStyle(mark).marginLeft).not.toBe("0px");
    const chipLeft = (of: Element) =>
      of
        .querySelector('[data-testid="status-chip"] .badge')!
        .getBoundingClientRect().left;
    const chip = row.querySelector('[data-testid="status-chip"] .badge')!;
    await expect(getComputedStyle(chip).marginLeft).toBe("0px");
    const others = [...canvasElement.querySelectorAll("tbody tr")].filter(
      (r) => r !== row,
    );
    await expect(others.length).toBeGreaterThan(0);
    for (const other of others)
      await expect(chipLeft(other)).toBeCloseTo(chipLeft(row), 1);
  },
};
export const DarkServersKeptOut: Story = {
  args: {
    ...ServersKeptOut.args,
    state: {
      ...fixture,
      settings: { ...fixture.settings, theme: "dark" },
      servers: fixture.servers.map((s) =>
        s.id === "a" ? { ...s, excluded: true } : s,
      ),
    },
  },
  play: ServersKeptOut.play,
};
/** The menu a right click on a server opens. */
export const ServerMenu: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const row = (
      await within(canvasElement).findByRole("button", { name: "Warsaw 03" })
    ).closest("tr")!;
    await userEvent.pointer({ keys: "[MouseRight]", target: row });
    await expect(
      await within(document.body).findByRole("menuitem", {
        name: "Исключить из автовыбора",
      }),
    ).toBeVisible();
  },
};
const failedSubscription: Snapshot = {
  ...fixture,
  servers: [],
  selected: null,
  status: "disconnected",
  runtime_active: false,
  subscriptions: [
    {
      ...fixture.subscriptions[0],
      count: 0,
      failure_code: "subscription_transport",
      error: "x",
    },
  ],
};
/** The subscription gave no servers: the reason and the way to try again, not "add a server". */
export const SubscriptionNoServers: Story = {
  args: { page: "servers", state: failedSubscription },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByRole("heading", {
        name: "Серверов пока нет: подписки не загрузились",
      }),
    ).toBeVisible();
  },
};
export const SubscriptionFailedSelected: Story = {
  args: { page: "servers", state: failedSubscription },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByRole("button", { name: /^Основная/ }));
    await expect(
      await c.findByRole("heading", {
        name: "Не удалось получить серверы подписки «Основная»",
      }),
    ).toBeVisible();
  },
};
/** The time a status was set, on hover. */
export const ServerStatusTip: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const row = (await c.findByRole("button", { name: "Warsaw 03" })).closest(
      "tr",
    )!;
    const chip = within(row).getByTestId("status-chip");
    const page = document.scrollingElement!;
    const height = page.scrollHeight;
    // The tip is out of the flow, fixed at the window, beside the mark and inside the window; it
    // adds nothing to the page. The same on hover and on keyboard focus.
    const beside = async () => {
      const tip = await within(document.body).findByRole("tooltip");
      await expect(tip).toBeVisible();
      await expect(getComputedStyle(tip).position).toBe("fixed");
      const mark = chip.getBoundingClientRect();
      const box = tip.getBoundingClientRect();
      await expect(box.top).toBeGreaterThanOrEqual(mark.bottom);
      await expect(box.top - mark.bottom).toBeLessThan(16);
      await expect(box.left).toBeGreaterThanOrEqual(0);
      await expect(box.right).toBeLessThanOrEqual(window.innerWidth);
      await expect(box.bottom).toBeLessThanOrEqual(window.innerHeight);
      await expect(page.scrollHeight).toBe(height);
    };
    await userEvent.hover(chip);
    await beside();
    await userEvent.unhover(chip);
    await expect(within(document.body).queryByRole("tooltip")).toBeNull();
    chip.focus();
    await beside();
  },
};
export const ServerDetails: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Подробности" }),
    );
    await expect(
      c.getByRole("columnheader", { name: "Протокол" }),
    ).toBeVisible();
  },
};
export const DarkServerDetails: Story = {
  args: {
    page: "servers",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
  play: ServerDetails.play,
};
/** Ordered by the delay through the server, upwards: the one that was not measured is last. */
export const ServersSorted: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      servers_sort: { column: "proxy_ms", descending: false },
    },
  },
  play: async (context) => {
    await ServerDetails.play?.(context);
    await expect(
      within(context.canvasElement).getByRole("columnheader", {
        name: "Через сервер",
      }),
    ).toHaveAttribute("aria-sort", "ascending");
  },
};
/** Ordered by a column only the detailed list has: the compact list says so in words. */
export const ServersSortedHidden: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      servers_sort: { column: "source", descending: true },
    },
  },
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getByText("Порядок: Источник, по убыванию"),
    ).toBeVisible();
  },
};
/** Backwards by status, in the dark theme. */
export const DarkServersSortedBack: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      settings: { ...fixture.settings, theme: "dark" },
      servers_sort: { column: "status", descending: true },
    },
  },
  play: async (context) => {
    await ServerDetails.play?.(context);
    await expect(
      within(context.canvasElement).getByRole("columnheader", {
        name: "Статус",
      }),
    ).toHaveAttribute("aria-sort", "descending");
  },
};
export const ConnectionDetails: Story = {
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByText("Подробности соединения"));
    await expect(
      c.getByRole("button", { name: "Копировать адрес прокси" }),
    ).toBeVisible();
  },
};
export const DarkConnectionDetails: Story = {
  args: {
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
  play: ConnectionDetails.play,
};
export const NoServers: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      servers: [],
      subscriptions: [],
      selected: null,
      status: "disconnected",
      runtime_active: false,
    },
  },
};
export const NoSubscriptions: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      subscriptions: [],
      servers: fixture.servers.filter((s) => !s.source),
      selected: "d",
    },
  },
};
/** A failed refresh as the core records it: the message and its typed code together. */
export const SubscriptionError: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      subscriptions: [
        {
          ...fixture.subscriptions[0],
          error: "Сетевой запрос не выполнен",
          failure_code: "subscription_transport",
        },
      ],
    },
  },
};
/** What a finished check leaves behind: every failed row carries the core's typed reason, and
 * the longest reasons sit on the rows with the most columns (a manual row has a delete icon). */
const failedChecks: Snapshot = {
  ...fixture,
  servers: [
    fixture.servers[0],
    {
      ...fixture.servers[1],
      proxy_ms: null,
      status: "failed",
      failure_code: "proxy_timeout",
    },
    {
      ...fixture.servers[2],
      endpoint_ms: null,
      proxy_ms: null,
      status: "unreachable",
      failure_code: "endpoint_timeout",
    },
    {
      ...fixture.servers[3],
      proxy_ms: null,
      status: "failed",
      failure_code: "proxy_invalid_response",
    },
  ],
  subscriptions: [
    {
      ...fixture.subscriptions[0],
      error: "Сетевой запрос не выполнен",
      failure_code: "subscription_timeout",
    },
    {
      ...fixture.subscriptions[1],
      error: "Сетевой запрос не выполнен",
      failure_code: "subscription_http_status",
    },
  ],
};
export const FailedChecks: Story = {
  args: { page: "servers", state: failedChecks },
};
export const DarkFailedChecks: Story = {
  args: {
    page: "servers",
    state: {
      ...failedChecks,
      settings: { ...failedChecks.settings, theme: "dark" },
    },
  },
};
/** Only the multi-word chips, no one-word `shadowsocks` row to fix the protocol column: the
 * table is wider than the panel at 1000 px, so a chip that may wrap is squeezed to its
 * longest word. Protocol and status chips outside the failure reason must stay on one line. */
const protocolChips: Snapshot = {
  ...fixture,
  servers: [
    fixture.servers[0],
    {
      ...fixture.servers[1],
      endpoint_ms: null,
      proxy_ms: null,
      status: "unchecked",
      checked_at: null,
    },
    { ...fixture.servers[2], proxy_ms: null, status: "reachable" },
    {
      ...fixture.servers[0],
      id: "e",
      name: "Vienna 04",
      source: null,
      endpoint_ms: null,
      proxy_ms: null,
      status: "unchecked",
      checked_at: null,
    },
  ],
};
export const ProtocolChips: Story = {
  args: { page: "servers", state: protocolChips },
  play: ServerDetails.play,
};
export const DarkProtocolChips: Story = {
  play: ServerDetails.play,
  args: {
    page: "servers",
    state: {
      ...protocolChips,
      settings: { ...protocolChips.settings, theme: "dark" },
    },
  },
};
/** Subscription refresh in progress: spinner on "Обновить все", per-source buttons disabled,
 * and the "Обновление подписок…" notice above the list. */
export const Refreshing: Story = {
  args: { page: "servers", state: { ...fixture, refreshing: true } },
};
export const DarkRefreshing: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      refreshing: true,
      settings: { ...fixture.settings, theme: "dark" },
    },
  },
};
export const Checking: Story = {
  args: { page: "servers", state: { ...fixture, checking: true } },
};
export const Settings: Story = { args: { page: "settings" } };
export const ProfileExportWarning: Story = {
  args: { page: "settings" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Экспортировать" }),
    );
    await expect(
      await c.findByRole("heading", { name: "Экспортировать профиль?" }),
    ).toBeVisible();
  },
};
export const ProfileImportPreview: Story = {
  args: { page: "settings" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Импортировать" }),
    );
    await expect(
      await c.findByRole("heading", { name: "Заменить текущий профиль?" }),
    ).toBeVisible();
  },
};
/** A restore on a clean install: the file's settings differ from the current ones. */
const restoredProfile = {
  settings: { ...fixture.settings, proxy_port: 3090, auto_select: false },
  servers: fixture.servers,
  subscriptions: fixture.subscriptions,
  selected: fixture.selected,
  routing: fixture.routing,
  servers_sort: fixture.servers_sort,
};
export const ProfileImported: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.profileFile = restoredProfile;
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Импортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Импортировать" }),
    );
    // The imported settings change the state while Settings is open: the screen must keep
    // its result and show the imported values.
    await expect(await c.findByText("Профиль импортирован.")).toBeVisible();
    await expect(c.getByLabelText("Порт", { exact: true })).toHaveValue(3090);
  },
};
/** The operation keeps running (a write or an apply that has not answered yet). */
export const ProfileExporting: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.holdProfile("export");
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Экспортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Экспортировать" }),
    );
    await expect(
      within(dialog).getByRole("button", { name: "Отмена" }),
    ).toBeDisabled();
    await expect(
      within(dialog).getByRole("button", { name: "Закрыть диалог" }),
    ).toBeDisabled();
  },
};
export const ProfileImporting: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.holdProfile("import");
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Импортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Импортировать" }),
    );
    await expect(
      within(dialog).getByRole("button", { name: "Отмена" }),
    ).toBeDisabled();
    await expect(
      within(dialog).getByRole("button", { name: "Закрыть диалог" }),
    ).toBeDisabled();
  },
};
const darkState: Snapshot = {
  ...fixture,
  settings: { ...fixture.settings, theme: "dark" },
};
/** Failures as the host reports them: the text for the dialog, and the core message that
 * also lands in `Snapshot.error` (the global banner keeps it). */
export const ProfileExportFailed: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.failProfile(
        "export",
        "Не удалось сохранить профиль",
        "Ошибка чтения или сохранения данных",
      );
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Экспортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Экспортировать" }),
    );
    await expect(
      await within(dialog).findByText("Не удалось сохранить профиль"),
    ).toBeVisible();
  },
};
export const DarkProfileImportFailed: Story = {
  args: {
    page: "settings",
    state: darkState,
    prepare: (backend) => {
      backend.failProfile(
        "import",
        "Не удалось применить профиль; текущие данные сохранены",
        "Ошибка чтения или сохранения данных",
      );
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Импортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Импортировать" }),
    );
    await expect(
      await within(dialog).findByText(
        "Не удалось применить профиль; текущие данные сохранены",
      ),
    ).toBeVisible();
  },
};
export const ProfileInspectFailed: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.failProfile(
        "inspect",
        "Некорректный формат профиля",
        "Некорректный формат данных",
      );
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Импортировать" }),
    );
    await expect(
      await c.findByText("Некорректный формат профиля"),
    ).toBeVisible();
  },
};
export const DarkProfileExported: Story = {
  args: { page: "settings", state: darkState },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Экспортировать" }),
    );
    const dialog = await c.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Экспортировать" }),
    );
    await expect(await c.findByText("Профиль экспортирован.")).toBeVisible();
  },
};
export const Journal: Story = { args: { page: "journal" } };
export const JournalExportWarning: Story = {
  args: { page: "journal" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Экспорт диагностики" }),
    );
    await expect(
      await c.findByRole("heading", {
        name: "Сохранить локальную диагностику?",
      }),
    ).toBeVisible();
  },
};
export const JournalDetails: Story = {
  args: {
    page: "journal",
    state: {
      ...fixture,
      journal: [
        {
          at: fixture.journal[0].at,
          level: "error",
          category: "subscriptions",
          message:
            "Подписка содержит некорректные данные; предыдущие серверы сохранены",
          details:
            "Xray parser: missing field settings.vnext[0].users at line 4 column 8",
        },
      ],
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(await c.findByText("Технические подробности"));
    await expect(
      c.getByText(
        "Xray parser: missing field settings.vnext[0].users at line 4 column 8",
      ),
    ).toBeVisible();
  },
};

/** Same-second records intentionally differ only in category/details, not timestamp. */
export const JournalDisclosureIdentity: Story = {
  args: {
    page: "journal",
    state: {
      ...fixture,
      journal: [
        {
          at: fixture.journal[0].at,
          category: "app",
          level: "error",
          message: "Ошибка интерфейса",
          details:
            "TypeError: boom\n    at refresh (assets/index-ab12.js:12:34)",
        },
        {
          at: fixture.journal[0].at,
          category: "subscriptions",
          level: "error",
          message: "Ошибка подписки",
          details: "field=settings.vnext",
        },
      ],
    },
  },
  render: function Render(args) {
    const backend = useMemo(() => new MockBackend(args.state), [args.state]);
    return (
      <BackendProvider client={backend}>
        <App initialPage="journal" />
        <button
          onClick={() =>
            backend.update((state) => {
              state.journal.push({
                at: state.journal[0].at + 1,
                category: "app",
                level: "error",
                message: "Новая ошибка",
                details: "stage=append",
              });
            })
          }
        >
          Добавить запись (сценарий)
        </button>
        <button
          onClick={() =>
            backend.update((state) => {
              state.journal.shift();
            })
          }
        >
          Ротация (сценарий)
        </button>
      </BackendProvider>
    );
  },
};
export const DarkJournalDisclosureIdentity: Story = {
  ...JournalDisclosureIdentity,
  args: {
    ...JournalDisclosureIdentity.args,
    state: {
      ...JournalDisclosureIdentity.args!.state!,
      settings: { ...fixture.settings, theme: "dark" },
    },
  },
};
export const ConnectInteraction: Story = {
  args: {
    state: { ...fixture, status: "disconnected", runtime_active: false },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Подключиться" }),
    );
    await expect(
      await c.findByRole("heading", { name: "Подключено" }),
    ).toBeVisible();
    await userEvent.click(c.getByRole("switch", { name: "Автовыбор сервера" }));
    await expect(c.getByRole("switch")).toHaveAttribute(
      "aria-checked",
      "false",
    );
  },
};
export const AddServerEmpty: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить сервер" }),
    );
  },
};
export const AddServerParsed: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить сервер" }),
    );
    await userEvent.type(c.getByLabelText("Ссылка сервера"), "vless://test");
    await expect(await c.findByText("Новый сервер")).toBeVisible();
  },
};
export const AddServerInvalid: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить сервер" }),
    );
    await userEvent.type(c.getByLabelText("Ссылка сервера"), "invalid");
    await expect(await c.findByRole("alert")).toBeVisible();
  },
};
export const AddSubscription: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить подписку" }),
    );
  },
};

const withProviderData = (theme: "light" | "dark" = "light") => ({
  ...fixture,
  settings: { ...fixture.settings, theme },
  subscriptions: [
    {
      ...fixture.subscriptions[0],
      name: "Основная",
      usage: {
        used: 42 * 1024 ** 3,
        total: 100 * 1024 ** 3,
        expire: new Date(2027, 0, 15, 12).getTime() / 1000,
      },
    },
    {
      ...fixture.subscriptions[1],
      availability: "exhausted" as const,
      usage: { used: 100 * 1024 ** 3, total: 100 * 1024 ** 3, expire: null },
    },
  ],
});
export const SubscriptionUsage: Story = {
  args: { page: "servers", state: withProviderData() },
};
export const DarkSubscriptionUsage: Story = {
  args: { page: "servers", state: withProviderData("dark") },
};
/** Opens the dialog that edits a subscription and waits for its address: the field has the focus. */
const editSubscription: Story["play"] = async ({ canvasElement }) => {
  const c = within(canvasElement);
  await userEvent.click(
    await c.findByRole("button", { name: "Изменить Основная" }),
  );
  const address = await within(document.body).findByLabelText(
    "Ссылка подписки",
  );
  await expect(address).toBeEnabled();
  await expect(address).toHaveFocus();
};
export const EditSubscriptionOptions: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      subscriptions: [
        {
          ...fixture.subscriptions[0],
          interval_mins: 30,
          user_agent: "Happ/1.0",
          filters: {
            include: ["DE", "NL"],
            exclude: ["test"],
            protocols: ["vless", "trojan"],
          },
        },
        fixture.subscriptions[1],
      ],
    },
  },
  play: editSubscription,
};

export const EditSubscriptionRoutesOff: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      subscriptions: [
        { ...fixture.subscriptions[0], provider_routes: false },
        fixture.subscriptions[1],
      ],
    },
  },
  play: editSubscription,
};

/** Ctrl+V on the page of servers, as the browser delivers it. */
function pasteText(text: string) {
  const data = new DataTransfer();
  data.setData("text", text);
  document.dispatchEvent(
    new ClipboardEvent("paste", {
      clipboardData: data,
      bubbles: true,
      cancelable: true,
    }),
  );
}
export const PastedServers: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("heading", { name: "Серверы" });
    pasteText(
      Array.from(
        { length: 8 },
        (_, i) => `vless://u@h${i}.example:443#Server ${i}`,
      ).join("\n"),
    );
    // The role query waits for the dialog to be open, not only to be in the page.
    const dialog = await within(document.body).findByRole("dialog");
    await expect(within(dialog).getByText("Найдено серверов: 8")).toBeVisible();
  },
};
export const PastedSubscription: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("heading", { name: "Серверы" });
    pasteText("Моя подписка: https://panel.example/sub?token=abc, спасибо");
    const dialog = await within(document.body).findByRole("dialog");
    await expect(
      within(dialog).getByDisplayValue("https://panel.example/sub?token=abc"),
    ).toBeVisible();
  },
};

const firstRun = {
  ...fixture,
  servers: [],
  subscriptions: [],
  selected: null,
  status: "disconnected",
  runtime_active: false,
  welcome: true,
};
export const Welcome: Story = {
  args: { state: firstRun },
  play: async ({ canvasElement }) => {
    const dialog = await within(canvasElement.ownerDocument.body).findByRole(
      "dialog",
    );
    await expect(
      within(dialog).getByText("Добро пожаловать в Rift"),
    ).toBeVisible();
  },
};
export const WelcomeRecognised: Story = {
  args: { state: firstRun },
  play: async ({ canvasElement }) => {
    const dialog = await within(canvasElement.ownerDocument.body).findByRole(
      "dialog",
    );
    await userEvent.type(
      within(dialog).getByLabelText("Ссылка подписки или сервера"),
      "https://panel.example/sub?token=abc",
    );
    await expect(
      await within(dialog).findByText("Подписка: panel.example"),
    ).toBeVisible();
  },
};
export const DarkWelcome: Story = {
  args: {
    state: { ...firstRun, settings: { ...firstRun.settings, theme: "dark" } },
  },
};

const mixedJournal = [
  {
    at: 1789742400,
    level: "info",
    message: "sing-box запущен",
    category: "core",
  },
  {
    at: 1789742401,
    level: "info",
    message: "Проверка серверов запущена",
    category: "checks",
  },
  {
    at: 1789742402,
    level: "info",
    message: "Проверка завершена: проверено 6, достижимо 3, рабочих 3",
    category: "checks",
  },
  {
    at: 1789742403,
    level: "info",
    message: "Рабочий сервер выбран",
    category: "checks",
  },
  {
    at: 1789742404,
    level: "error",
    message: "Не удалось обновить подписку; предыдущие серверы сохранены",
    category: "subscriptions",
    failure_code: "subscription_timeout",
  },
  {
    at: 1789742405,
    level: "info",
    message: "Серверов добавлено: 2",
    category: "app",
  },
] as const;
export const JournalBySource: Story = {
  args: {
    page: "journal",
    state: { ...fixture, journal: mixedJournal.map((entry) => ({ ...entry })) },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Проверки серверов" }),
    );
    await expect(
      await c.findByText("Проверка серверов запущена"),
    ).toBeVisible();
  },
};
export const JournalHistory: Story = {
  args: { page: "journal" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Проверки серверов" }),
    );
    await userEvent.click(await c.findByText("Замеры серверов"));
    await expect(
      await c.findByRole("table", { name: "Последние замеры сервера" }),
    ).toBeVisible();
  },
};
const connectionRows: ConnectionList = {
  total: 812,
  items: [
    {
      host: "www.example.com",
      port: 443,
      network: "tcp",
      route: "proxy",
      server: "Frankfurt 01",
      process: "chrome.exe",
      upload: 18 * 1024,
      download: 2.4 * 1024 ** 2,
      seconds: 42,
    },
    {
      host: "cdn.example.net",
      port: 443,
      network: "tcp",
      route: "proxy",
      server: "Frankfurt 01",
      process: "chrome.exe",
      upload: 3 * 1024,
      download: 640 * 1024,
      seconds: 130,
    },
    {
      host: "ya.ru",
      port: 443,
      network: "tcp",
      route: "direct",
      server: null,
      process: "firefox.exe",
      upload: 9 * 1024,
      download: 120 * 1024,
      seconds: 3700,
    },
    {
      host: "192.0.2.53",
      port: 53,
      network: "udp",
      route: "direct",
      server: null,
      process: null,
      upload: 120,
      download: 240,
      seconds: 4,
    },
  ],
};
function showConnections(backend: MockBackend) {
  backend.connections = connectionRows;
}
export const Connections: Story = {
  args: { page: "connections", prepare: showConnections },
  play: async ({ canvasElement }) => {
    await expect(await within(canvasElement).findByRole("table")).toBeVisible();
  },
};
export const DarkConnections: Story = {
  args: {
    page: "connections",
    prepare: showConnections,
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
  play: Connections.play,
};
/** Ordered by the received bytes, downwards. */
export const ConnectionsSorted: Story = {
  args: { page: "connections", prepare: showConnections },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const heading = await c.findByRole("columnheader", { name: "Получено" });
    await userEvent.click(within(heading).getByRole("button"));
    await userEvent.click(within(heading).getByRole("button"));
    await expect(heading).toHaveAttribute("aria-sort", "descending");
  },
};
export const ConnectionsNotConnected: Story = {
  args: {
    page: "connections",
    state: { ...fixture, status: "disconnected", runtime_active: false },
  },
};
export const JournalAllSources: Story = {
  args: {
    page: "journal",
    state: { ...fixture, journal: mixedJournal.map((entry) => ({ ...entry })) },
  },
};

export const Routing: Story = { args: { page: "routing" } };
export const DarkRouting: Story = {
  args: {
    page: "routing",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};
export const RoutingEdited: Story = {
  args: { page: "routing" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.selectOptions(
      await c.findByRole("combobox", { name: "Порядок проверки" }),
      "Proxy → Direct → Block",
    );
    await userEvent.type(
      c.getByRole("textbox", { name: "Правила Proxy" }),
      "domain:work.example{enter}process:chrome.exe",
    );
    await expect(await c.findByText(/Последним стоит Block/)).toBeVisible();
  },
};
export const RoutingDatabases: Story = {
  args: {
    page: "routing",
    state: {
      ...fixture,
      geodata: {
        ...fixture.geodata,
        geosite: {
          updated_at: 1789742400,
          bytes: 11_205_406,
          checked_at: 1789828800,
        },
        geoip: { updated_at: 1789742400, bytes: 16_587_403 },
        failure_code: "geodata_download",
      },
    },
  },
};

export const DarkServers: Story = {
  args: {
    page: "servers",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};
export const DarkSettings: Story = {
  args: {
    page: "settings",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};
export const DarkJournal: Story = {
  args: {
    page: "journal",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};
export const Manual: Story = {
  args: {
    state: {
      ...fixture,
      settings: { ...fixture.settings, auto_select: false },
    },
  },
};
/** The banner arrives as a live event after the window has subscribed, like in production. */
export const UpdateAvailable: Story = {
  args: {
    prepare: (backend) => {
      setTimeout(
        () =>
          backend.pushUpdate({
            version: nextVersion,
            notes: null,
            pub_date: null,
          }),
        300,
      );
    },
  },
};
/** Errors the host reports arrive as live events: toasts bottom right, no window. */
export const ErrorToast: Story = {
  args: {
    prepare: (backend) => {
      setTimeout(
        () => backend.raiseError("Не удалось обновить подписку «Основная»"),
        100,
      );
    },
  },
  play: async ({ canvasElement }) => {
    const toast = await within(canvasElement).findByRole("alert");
    await expect(toast).toHaveTextContent("Не удалось обновить подписку");
    await expect(
      within(canvasElement).queryByRole("dialog"),
    ).not.toBeInTheDocument();
  },
};
/** A notice of the host (a fallback that took place) is an information toast: no failure. */
export const NoticeToast: Story = {
  args: {
    prepare: (backend) => {
      setTimeout(
        () =>
          backend.raiseNotice(
            "Подтверждение Windows не получено: включён режим Proxy",
          ),
        100,
      );
    },
  },
  play: async ({ canvasElement }) => {
    const text = await within(canvasElement).findByText(/включён режим Proxy/);
    await expect(text.closest(".toast")).toHaveClass("info");
  },
};
export const DarkErrorToasts: Story = {
  args: {
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
    prepare: (backend) => {
      setTimeout(() => {
        backend.raiseError("Не удалось обновить подписку «Основная»");
        backend.raiseError(
          `Внутренняя ошибка Rift: panicked at core/src/application.rs:712:9 ${"frame ".repeat(80)}`,
        );
      }, 100);
    },
  },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findAllByRole("alert"),
    ).toHaveLength(2);
  },
};
/** Before the first snapshot arrives the window shows only the shell and a loading line. */
export const Starting: Story = {
  args: {
    prepare: (backend) => {
      backend.snapshot = () => new Promise<Snapshot>(() => {});
    },
  },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText("Подключение к Rift…"),
    ).toBeVisible();
  },
};
export const ConfirmRemoveSubscription: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Удалить Основная" }),
    );
    await expect(
      await c.findByRole("heading", { name: "Удалить «Основная»?" }),
    ).toBeVisible();
  },
};
export const TunMode: Story = {
  args: {
    state: {
      ...fixture,
      settings: { ...fixture.settings, mode: "tun" },
    },
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByText("Подробности соединения"),
    );
    await expect(
      await within(canvasElement).findByText("Выключен в режиме TUN"),
    ).toBeVisible();
  },
};

const longJournal: Snapshot = {
  ...fixture,
  journal: [
    {
      at: 1789742400,
      level: "warn",
      category: "core",
      message: "Безопасное сообщение журнала ".repeat(100),
      failure_code: null,
    },
  ],
};
export const JournalLongMessage: Story = {
  args: { page: "journal", state: longJournal },
};
export const DarkJournalLongMessage: Story = {
  args: {
    page: "journal",
    state: {
      ...longJournal,
      settings: { ...longJournal.settings, theme: "dark" },
    },
  },
};
export const DarkWelcomeRecognised: Story = {
  ...WelcomeRecognised,
  args: {
    state: { ...firstRun, settings: { ...firstRun.settings, theme: "dark" } },
  },
};
export const DarkAddSubscription: Story = {
  ...AddSubscription,
  args: {
    page: "servers",
    state: { ...fixture, settings: { ...fixture.settings, theme: "dark" } },
  },
};

/** Provider names stay plain Unicode text; one offline font covers every production surface. */
const flagNames = ["🇫🇷 Paris · 🚀", "🇵🇱 Warsaw · ✨"];
const regionalFlags: Snapshot = {
  ...fixture,
  servers: fixture.servers.map((server, index) => ({
    ...server,
    name: flagNames[index] ?? server.name,
  })),
};
const darkRegionalFlags: Snapshot = {
  ...regionalFlags,
  settings: { ...regionalFlags.settings, theme: "dark" },
};
async function flagFontReady() {
  const faces = await document.fonts.load(
    '16px "Twemoji Country Flags"',
    "🇫🇷🇵🇱",
  );
  await expect(faces.length).toBeGreaterThan(0);
  for (const face of faces) await expect(face.status).toBe("loaded");
}
export const RegionalFlags: Story = {
  args: { page: "servers", state: regionalFlags },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    for (const name of flagNames)
      await expect(await c.findByRole("button", { name })).toBeVisible();
    await expect(
      c.queryByRole("switch", { name: "Включена" }),
    ).not.toBeInTheDocument();
    await flagFontReady();
  },
};
export const DarkRegionalFlags: Story = {
  ...RegionalFlags,
  args: { page: "servers", state: darkRegionalFlags },
};
export const RegionalFlagsConnection: Story = {
  args: { state: regionalFlags },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(flagNames[0]),
    ).toBeVisible();
    await flagFontReady();
  },
};
export const DarkRegionalFlagsConnection: Story = {
  ...RegionalFlagsConnection,
  args: { state: darkRegionalFlags },
};
export const RegionalFlagsHistory: Story = {
  args: { page: "journal", state: regionalFlags },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Проверки серверов" }),
    );
    await userEvent.click(c.getByText("Замеры серверов"));
    await expect(c.getByRole("combobox", { name: "Сервер" })).toHaveValue("a");
    for (const name of flagNames)
      await expect(c.getByRole("option", { name })).toBeInTheDocument();
    await flagFontReady();
  },
};
export const DarkRegionalFlagsHistory: Story = {
  ...RegionalFlagsHistory,
  args: { page: "journal", state: darkRegionalFlags },
};
export const RegionalFlagsConnections: Story = {
  args: {
    page: "connections",
    state: regionalFlags,
    prepare: (backend) => {
      backend.connections = {
        total: 2,
        items: flagNames.map<ConnectionRow>((name, index) => ({
          host: `example-${index}.test`,
          port: 443,
          network: "tcp",
          route: "proxy",
          server: name,
          process: "browser.exe",
          upload: 1024,
          download: 4096,
          seconds: 10,
        })),
      };
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    for (const name of flagNames)
      await expect(await c.findByRole("cell", { name })).toBeVisible();
    // Searching consumes the same original name, not a country code or an image label.
    await userEvent.type(c.getByLabelText("Поиск по соединениям"), "🇵🇱");
    await expect(
      c.queryByRole("cell", { name: flagNames[0] }),
    ).not.toBeInTheDocument();
    await expect(c.getByRole("cell", { name: flagNames[1] })).toBeVisible();
    await userEvent.clear(c.getByLabelText("Поиск по соединениям"));
    await flagFontReady();
  },
};
export const DarkRegionalFlagsConnections: Story = {
  ...RegionalFlagsConnections,
  args: { ...RegionalFlagsConnections.args, state: darkRegionalFlags },
};
export const RegionalFlagsImport: Story = {
  args: {
    page: "servers",
    state: regionalFlags,
    prepare: (backend) => {
      const preview = backend.preview.bind(backend);
      backend.preview = async (uri) => ({
        ...(await preview(uri)),
        name: flagNames[0],
      });
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить сервер" }),
    );
    await userEvent.type(c.getByLabelText("Ссылка сервера"), "vless://test");
    await expect(
      await c.findByRole("heading", { name: flagNames[0] }),
    ).toBeVisible();
    await expect(
      within(c.getByRole("dialog")).getByRole("button", { name: "Добавить" }),
    ).toBeEnabled();
    await flagFontReady();
  },
};
export const DarkRegionalFlagsImport: Story = {
  ...RegionalFlagsImport,
  args: { ...RegionalFlagsImport.args, state: darkRegionalFlags },
};
export const AddServerChangedInput: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(
      await c.findByRole("button", { name: "Добавить сервер" }),
    );
    const field = c.getByLabelText("Ссылка сервера");
    const add = within(c.getByRole("dialog")).getByRole("button", {
      name: "Добавить",
    });
    await userEvent.type(field, "vless://test");
    await expect(
      await c.findByRole("heading", { name: "Новый сервер" }),
    ).toBeVisible();
    await expect(add).toBeEnabled();
    await userEvent.clear(field);
    await expect(add).toBeDisabled();
    await expect(
      c.queryByRole("heading", { name: "Новый сервер" }),
    ).not.toBeInTheDocument();
    await userEvent.type(field, "invalid");
    await expect(
      await within(c.getByRole("dialog")).findByRole("alert"),
    ).toHaveTextContent("Некорректный URI сервера");
    await expect(add).toBeDisabled();
  },
};
export const DarkAddServerChangedInput: Story = {
  ...AddServerChangedInput,
  args: { page: "servers", state: darkState },
};
export const DarkAddServerParsed: Story = {
  ...AddServerParsed,
  args: { page: "servers", state: darkState },
};
export const JournalLevelCentered: Story = {
  args: { page: "journal" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const heading = await c.findByRole("columnheader", { name: "Уровень" });
    const table = heading.closest("table")!;
    await expect(getComputedStyle(heading).textAlign).toBe("center");
    const index = (heading as HTMLTableCellElement).cellIndex;
    for (const row of table.querySelectorAll("tbody tr")) {
      const cells = [...row.querySelectorAll("td")];
      await expect(getComputedStyle(cells[index]).textAlign).toBe("center");
      for (const [column, cell] of cells.entries())
        if (column !== index)
          await expect(getComputedStyle(cell).textAlign).toBe("left");
    }
    for (const other of table.querySelectorAll("th"))
      if (other !== heading)
        await expect(getComputedStyle(other).textAlign).toBe("left");
  },
};
export const DarkJournalLevelCentered: Story = {
  ...JournalLevelCentered,
  args: { page: "journal", state: darkState },
};

const startedLatePreview = fn();
/** Hold A until B has answered, then deliver its stale success or failure without timers. */
function latePreview(outcome: "success" | "error") {
  return (backend: MockBackend) => {
    startedLatePreview.mockClear();
    const preview = backend.preview.bind(backend);
    let finishA: (() => void) | null = null;
    backend.preview = (uri) => {
      if (uri === "vless://A") {
        startedLatePreview(uri);
        return new Promise<Server>((resolve, reject) => {
          finishA = () => {
            if (outcome === "success")
              resolve({ ...fixture.servers[0], name: "Устаревший сервер A" });
            else reject("Ошибка старой ссылки A");
          };
        });
      }
      if (uri === "vless://B") {
        const result = preview(uri).then((server) => ({
          ...server,
          name: "Текущий сервер B",
        }));
        // B's existing subscribers accept its result before the old request settles.
        void result.then(() => queueMicrotask(() => finishA?.()));
        return result;
      }
      return preview(uri);
    };
  };
}
const latePreviewInteraction: Story["play"] = async ({ canvasElement }) => {
  const c = within(canvasElement);
  await userEvent.click(
    await c.findByRole("button", { name: "Добавить сервер" }),
  );
  const field = c.getByLabelText("Ссылка сервера");
  const dialog = within(c.getByRole("dialog"));
  const add = dialog.getByRole("button", { name: "Добавить" });
  await userEvent.type(field, "vless://A");
  await expect(dialog.getByRole("status")).toHaveTextContent(
    "Распознаём ссылку…",
  );
  await expect(add).toBeDisabled();
  await waitFor(() => {
    expect(startedLatePreview).toHaveBeenCalledTimes(1);
    expect(startedLatePreview).toHaveBeenCalledWith("vless://A");
  });
  await userEvent.clear(field);
  await userEvent.type(field, "vless://B");
  await expect(
    await c.findByRole("heading", { name: "Текущий сервер B" }),
  ).toBeVisible();
  await expect(
    c.queryByRole("heading", { name: "Устаревший сервер A" }),
  ).not.toBeInTheDocument();
  await expect(dialog.queryByRole("alert")).not.toBeInTheDocument();
  await expect(add).toBeEnabled();
};
export const AddServerLatePreviewSuccess: Story = {
  args: { page: "servers", prepare: latePreview("success") },
  play: latePreviewInteraction,
};
export const DarkAddServerLatePreviewSuccess: Story = {
  ...AddServerLatePreviewSuccess,
  args: { ...AddServerLatePreviewSuccess.args, state: darkState },
};
export const AddServerLatePreviewError: Story = {
  args: { page: "servers", prepare: latePreview("error") },
  play: latePreviewInteraction,
};
export const DarkAddServerLatePreviewError: Story = {
  ...AddServerLatePreviewError,
  args: { ...AddServerLatePreviewError.args, state: darkState },
};

export const ActiveSubscriptions: Story = {
  args: { page: "servers" },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await c.findByRole("button", { name: "Основная 2" });
    await expect(
      c.queryByRole("switch", { name: "Включена" }),
    ).not.toBeInTheDocument();
    await userEvent.click(c.getByRole("button", { name: "Изменить Основная" }));
    const dialog = within(await c.findByRole("dialog"));
    await expect(
      dialog.queryByRole("switch", { name: "Включена" }),
    ).not.toBeInTheDocument();
    await userEvent.click(dialog.getByText("Дополнительно"));
    await expect(
      dialog.getByRole("switch", { name: "Применять маршруты провайдера" }),
    ).toBeChecked();
  },
};
export const DarkActiveSubscriptions: Story = {
  ...ActiveSubscriptions,
  args: { page: "servers", state: darkState },
};

/** Every typed legacy/Xray failure is rendered through the production failure formatter. */
const engineFailures = [
  [
    "unsupported_legacy_parameters",
    "Параметры старого сервера больше не поддерживаются",
  ],
  ["xray_config_rejected", "Xray отклонил параметры сервера"],
  ["xray_config_timeout", "Проверка Xray превысила время ожидания"],
  ["xray_start_exit", "Не удалось запустить Xray"],
  ["xray_readiness_timeout", "Локальный канал Xray не отвечает"],
  ["xray_crashed", "Xray неожиданно завершился"],
] as const;
export const EngineFailures: Story = {
  args: {
    page: "servers",
    state: {
      ...fixture,
      selected: null,
      status: "disconnected",
      runtime_active: false,
      subscriptions: [],
      servers: engineFailures.map(([failure_code], index) => ({
        ...fixture.servers[0],
        id: `engine-${index}`,
        name: `Xray ${index + 1}`,
        source: null,
        status: "failed",
        endpoint_ms: null,
        proxy_ms: null,
        failure_code,
      })),
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    for (const [, message] of engineFailures)
      await expect(await c.findByText(message)).toBeVisible();
  },
};

function darkVariant(story: Story): Story {
  const state = story.args?.state ?? fixture;
  return {
    ...story,
    args: {
      ...story.args,
      state: { ...state, settings: { ...state.settings, theme: "dark" } },
    },
  };
}
export const DarkEngineFailures: Story = { ...darkVariant(EngineFailures) };
export const DarkJournalDetails: Story = { ...darkVariant(JournalDetails) };
export const DarkJournalExportWarning: Story = {
  ...darkVariant(JournalExportWarning),
};

/** Equal diagnostic payloads remain distinct when their crash identities differ. */
export const JournalCrashIdentity: Story = {
  ...JournalDisclosureIdentity,
  args: {
    page: "journal",
    state: {
      ...fixture,
      journal: ["crash-story-a", "crash-story-b"].map((crash_id) => ({
        at: fixture.journal[0].at,
        category: "app",
        level: "error",
        message: "Ошибка интерфейса",
        details: "TypeError: repeated crash",
        crash_id,
      })),
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const disclosures = await c.findAllByText("Технические подробности");
    await userEvent.click(disclosures[0]);
    await expect(disclosures[0].closest("details")).toHaveAttribute("open");
    await expect(disclosures[1].closest("details")).not.toHaveAttribute("open");
    await userEvent.click(
      c.getByRole("button", { name: "Добавить запись (сценарий)" }),
    );
    await expect(
      c.getAllByText("Технические подробности")[0].closest("details"),
    ).toHaveAttribute("open");
    await userEvent.click(
      c.getByRole("button", { name: "Ротация (сценарий)" }),
    );
    // Removing crash A must not transfer its disclosure state to the identical crash B.
    await expect(
      c.getAllByText("Технические подробности")[0].closest("details"),
    ).not.toHaveAttribute("open");
    await expect(c.getByText("TypeError: repeated crash")).not.toBeVisible();
  },
};
export const DarkJournalCrashIdentity: Story = {
  ...darkVariant(JournalCrashIdentity),
};

function emptySubscription(
  failure_code: string | null,
  title: string,
  refreshing = false,
): Story {
  return {
    args: {
      page: "servers",
      state: {
        ...failedSubscription,
        refreshing,
        subscriptions: [
          {
            ...fixture.subscriptions[0],
            count: 0,
            updated_at: null,
            failure_code,
            error: null,
            filters:
              failure_code === "subscription_filtered"
                ? {
                    include: ["No matching region"],
                    exclude: [],
                    protocols: [],
                  }
                : fixture.subscriptions[0].filters,
          },
        ],
      },
    },
    play: async ({ canvasElement }) => {
      const c = within(canvasElement);
      await userEvent.click(
        await c.findByRole("button", { name: /^Основная/ }),
      );
      await expect(
        await c.findByRole("heading", { name: title }),
      ).toBeVisible();
      if (refreshing)
        await expect(
          c.getByRole("button", { name: "Обновить все" }),
        ).toBeDisabled();
      else
        await expect(
          c.getByRole("button", {
            name:
              failure_code === "subscription_filtered"
                ? "Изменить подписку"
                : "Обновить",
          }),
        ).toBeEnabled();
    },
  };
}
export const SubscriptionFiltered: Story = {
  ...emptySubscription(
    "subscription_filtered",
    "Фильтры скрыли все серверы подписки",
  ),
};
export const SubscriptionInvalid: Story = {
  ...emptySubscription(
    "subscription_invalid_body",
    "Подписка не содержит серверов",
  ),
};
export const SubscriptionNeverLoaded: Story = {
  ...emptySubscription(null, "Серверы подписки ещё не загружены"),
};
export const SubscriptionLoading: Story = {
  ...emptySubscription(null, "Загружаем серверы…", true),
};
export const DarkSubscriptionFiltered: Story = {
  ...darkVariant(SubscriptionFiltered),
};
export const DarkSubscriptionInvalid: Story = {
  ...darkVariant(SubscriptionInvalid),
};
export const DarkSubscriptionNeverLoaded: Story = {
  ...darkVariant(SubscriptionNeverLoaded),
};
export const DarkSubscriptionLoading: Story = {
  ...darkVariant(SubscriptionLoading),
};

export const ConnectionsLoading: Story = {
  args: { page: "connections" },
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText("Читаем соединения…"),
    ).toBeVisible();
  },
};
export const ConnectionsEmpty: Story = {
  args: {
    page: "connections",
    prepare: (backend) => {
      backend.connections = { total: 0, items: [] };
    },
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(
      await c.findByRole("heading", { name: "Активных соединений нет" }),
    ).toBeVisible();
    await expect(c.queryByText("Нет подключения")).not.toBeInTheDocument();
  },
};
export const ConnectionsNoMatches: Story = {
  ...Connections,
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await c.findByRole("table");
    await userEvent.type(
      c.getByLabelText("Поиск по соединениям"),
      "no-such-host.example",
    );
    await expect(
      await c.findByRole("heading", { name: "Ничего не найдено" }),
    ).toBeVisible();
    await expect(c.getByText("Измените поиск.")).toBeVisible();
  },
};
export const DarkConnectionsLoading: Story = {
  ...darkVariant(ConnectionsLoading),
};
export const DarkConnectionsEmpty: Story = { ...darkVariant(ConnectionsEmpty) };
export const DarkConnectionsNoMatches: Story = {
  ...darkVariant(ConnectionsNoMatches),
};

const importedServers: InputPreview = {
  kind: "servers",
  rejected: 0,
  items: [
    {
      uri: coreContract.server_uri,
      server: {
        ...coreContract.unchecked_server,
      },
    },
  ],
};
async function expectIncomingServers(canvasElement: HTMLElement) {
  const dialog = await within(canvasElement.ownerDocument.body).findByRole(
    "dialog",
  );
  const c = within(dialog);
  await expect(
    c.getByRole("heading", { name: "Добавить серверы" }),
  ).toBeVisible();
  await expect(
    c.getByRole("heading", { name: coreContract.unchecked_server.name }),
  ).toBeVisible();
  await expect(c.getByRole("button", { name: "Добавить" })).toBeEnabled();
}
export const QrFileImport: Story = {
  args: {
    page: "servers",
    prepare: (backend) => {
      backend.qr = importedServers;
    },
  },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", { name: "QR из файла" }),
    );
    await expectIncomingServers(canvasElement);
  },
};
export const QrClipboardImport: Story = {
  ...QrFileImport,
  play: async ({ canvasElement }) => {
    await userEvent.click(
      await within(canvasElement).findByRole("button", {
        name: "QR из буфера",
      }),
    );
    await expectIncomingServers(canvasElement);
  },
};
export const PendingDeepLinkImport: Story = {
  args: {
    prepare: (backend) => {
      backend.pendingImport = importedServers;
    },
  },
  play: async ({ canvasElement }) => {
    await expectIncomingServers(canvasElement);
    await expect(
      within(canvasElement).getByRole("heading", { name: "Серверы" }),
    ).toBeVisible();
  },
};
export const LiveDeepLinkImport: Story = {
  loaders: [() => ({ backend: new MockBackend() })],
  render: (_, { loaded }) => (
    <BackendProvider client={loaded.backend}>
      <App />
    </BackendProvider>
  ),
  play: async ({ canvasElement, loaded }) => {
    const c = within(canvasElement);
    await c.findByRole("heading", { name: "Подключено" });
    loaded.backend.pushImport(importedServers);
    await expectIncomingServers(canvasElement);
    await expect(c.getByRole("heading", { name: "Серверы" })).toBeVisible();
  },
};
export const DarkQrFileImport: Story = { ...darkVariant(QrFileImport) };
export const DarkQrClipboardImport: Story = {
  ...darkVariant(QrClipboardImport),
};
export const DarkPendingDeepLinkImport: Story = {
  ...darkVariant(PendingDeepLinkImport),
};
export const DarkLiveDeepLinkImport: Story = {
  ...LiveDeepLinkImport,
  loaders: [
    () => ({
      backend: new MockBackend({
        ...fixture,
        settings: { ...fixture.settings, theme: "dark" },
      }),
    }),
  ],
};

const checkUpdate: Story["play"] = async ({ canvasElement }) => {
  await userEvent.click(
    await within(canvasElement).findByRole("button", {
      name: "Проверить обновления",
    }),
  );
};
export const UpdateChecking: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.checkForUpdate = () => new Promise(() => {});
    },
  },
  play: async (context) => {
    await checkUpdate(context);
    const c = within(context.canvasElement);
    await expect(
      c.getByRole("button", { name: "Проверить обновления" }),
    ).toBeDisabled();
    await expect(
      c.queryByText("У вас последняя версия."),
    ).not.toBeInTheDocument();
  },
};
export const UpdateCheckError: Story = {
  args: {
    page: "settings",
    prepare: (backend) => {
      backend.checkForUpdate = () => Promise.reject("Сеть недоступна");
    },
  },
  play: async (context) => {
    await checkUpdate(context);
    const c = within(context.canvasElement);
    await expect(await c.findByText("Сеть недоступна")).toBeVisible();
    await expect(
      c.getByRole("button", { name: "Проверить обновления" }),
    ).toBeEnabled();
    await expect(
      c.queryByText("У вас последняя версия."),
    ).not.toBeInTheDocument();
  },
};
export const UpdateNoUpdate: Story = {
  args: { page: "settings" },
  play: async (context) => {
    await checkUpdate(context);
    await expect(
      await within(context.canvasElement).findByText("У вас последняя версия."),
    ).toBeVisible();
  },
};
export const DarkUpdateChecking: Story = { ...darkVariant(UpdateChecking) };
export const DarkUpdateCheckError: Story = { ...darkVariant(UpdateCheckError) };
export const DarkUpdateNoUpdate: Story = { ...darkVariant(UpdateNoUpdate) };

/** The visual harness changes real browser media while this story stays mounted. */
export const SystemTheme: Story = {
  args: {
    state: { ...fixture, settings: { ...fixture.settings, theme: "system" } },
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("heading", { name: "Подключено" });
    await waitFor(() =>
      expect(document.documentElement).toHaveAttribute(
        "data-theme",
        matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
      ),
    );
  },
};
