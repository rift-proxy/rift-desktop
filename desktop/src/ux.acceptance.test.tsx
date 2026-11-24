import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { App, type Page } from "./App";
import type { Snapshot } from "./backend/types";
import { BackendProvider } from "./backend/provider";
import { ServerTable } from "./components/servers";
import { checkedAt, time } from "./format";
import { MockBackend, fixture } from "./testing/mock";

function mount(state: Snapshot = fixture, page: Page = "connection") {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  const view = render(
    <BackendProvider client={backend}>
      <App initialPage={page} />
    </BackendProvider>,
  );
  return { backend, execute, ...view };
}

function mountRow(detailed = false) {
  const onSelect = vi.fn();
  const onDelete = vi.fn();
  render(
    <ServerTable
      servers={[fixture.servers[3]]}
      selected={null}
      sources={fixture.subscriptions}
      sort={fixture.servers_sort}
      onSort={vi.fn()}
      detailed={detailed}
      onSelect={onSelect}
      onDelete={onDelete}
    />,
  );
  const row = screen.getByRole("button", { name: "Warsaw 03" }).closest("tr")!;
  return { row, onSelect, onDelete };
}

it.each([false, true])(
  "makes subscription the primary add action, including empty=%s",
  async (empty) => {
    const user = userEvent.setup();
    mount(
      empty
        ? { ...fixture, servers: [], selected: null, subscriptions: [] }
        : fixture,
      "servers",
    );
    const subscription = await screen.findAllByRole("button", {
      name: "Добавить подписку",
    });
    expect(
      subscription.some((button) => button.classList.contains("primary")),
    ).toBe(true);
    expect(
      screen
        .getAllByRole("button", { name: "Добавить сервер" })
        .some((button) => button.classList.contains("primary")),
    ).toBe(false);
    await user.click(subscription[0]);
    const dialog = screen.getByRole("dialog");
    const url = within(dialog).getByLabelText(/^(URL|Ссылка) подписки$/);
    expect(within(dialog).getAllByRole("textbox")[0]).toBe(url);
    expect(url).toHaveFocus();
    // URL-first still permits completing the existing form without an extra step.
    await user.type(url, "https://example.com/subscription");
    // The name is not asked for: the subscription names itself.
    expect(within(dialog).queryByLabelText("Название")).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    // Until its provider names it, the host of the link stands in.
    expect(
      screen.getByRole("button", { name: /^example\.com \d+$/ }),
    ).toBeVisible();
  },
);

it.each([false, true])(
  "selects exactly once from every server cell, detailed=%s",
  async (detailed) => {
    const user = userEvent.setup();
    const { row, onSelect, onDelete } = mountRow(detailed);
    const cells = within(row).getAllByRole("cell");
    expect(cells.length).toBeGreaterThan(3);
    for (const cell of cells) {
      onSelect.mockClear();
      await user.click(cell);
      expect(
        onSelect,
        `cell: ${cell.textContent}`,
      ).toHaveBeenCalledExactlyOnceWith("d");
      expect(onDelete).not.toHaveBeenCalled();
    }
  },
);

it("uses the existing Select action once when a server cell is clicked", async () => {
  const user = userEvent.setup();
  const { execute, backend } = mount(fixture, "servers");
  const name = await screen.findByRole("button", { name: "Warsaw 03" });
  const row = name.closest("tr")!;
  await user.click(within(row).getByText("74 мс", { exact: true }));
  await waitFor(() =>
    expect(execute).toHaveBeenCalledExactlyOnceWith({
      type: "select",
      id: "d",
    }),
  );
  expect(await backend.snapshot()).toMatchObject({
    selected: "d",
    runtime_active: true,
    // A choice by hand is a server in use like another: Auto Select stays on.
    settings: { auto_select: true },
  });
});

it.each(["{Enter}", " "])(
  "keyboard %s on the server name selects once",
  async (key) => {
    const user = userEvent.setup();
    const { onSelect } = mountRow();
    screen.getByRole("button", { name: "Warsaw 03" }).focus();
    await user.keyboard(key);
    expect(onSelect).toHaveBeenCalledExactlyOnceWith("d");
  },
);

it("does not select when the pointer is dragged across the row, then permits the next ordinary click", () => {
  const { row, onSelect } = mountRow();
  const name = screen.getByRole("button", { name: "Warsaw 03" });
  const cell = within(row).getAllByRole("cell")[2];
  // Press on the name, release in another cell: the browser clicks the common ancestor.
  fireEvent.pointerDown(name, { clientX: 40, clientY: 20 });
  fireEvent.click(row, { detail: 1, clientX: 260, clientY: 22 });
  fireEvent.pointerDown(cell, { clientX: 260, clientY: 20 });
  fireEvent.click(name, { detail: 1, clientX: 40, clientY: 24 });
  expect(onSelect).not.toHaveBeenCalled();
  // A few pixels of hand shake are still a click.
  fireEvent.pointerDown(cell, { clientX: 100, clientY: 20 });
  fireEvent.click(cell, { detail: 1, clientX: 102, clientY: 21 });
  expect(onSelect).toHaveBeenCalledExactlyOnceWith("d");
});

it("only asks for deletion, and confirms without selecting the manual server", async () => {
  const user = userEvent.setup();
  const { execute } = mount(fixture, "servers");
  await user.click(
    await screen.findByRole("button", { name: "Удалить Warsaw 03" }),
  );
  expect(screen.getByRole("dialog")).toBeVisible();
  expect(execute).not.toHaveBeenCalled();
  await user.click(
    within(screen.getByRole("dialog")).getByRole("button", {
      name: "Удалить",
    }),
  );
  await waitFor(() =>
    expect(execute).toHaveBeenCalledExactlyOnceWith({
      type: "remove_server",
      id: "d",
    }),
  );
  expect(
    screen.queryByRole("button", { name: "Warsaw 03" }),
  ).not.toBeInTheDocument();
});

it.each([false, true])(
  "offers recovery for active=%s and keeps the full core error through navigation",
  async (active) => {
    const user = userEvent.setup();
    const coreError = "Сбой проверки: исходное сообщение ядра — код 503";
    const { execute } = mount({
      ...fixture,
      status: "error",
      runtime_active: active,
      error: coreError,
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(coreError);
    const notice = screen.getByRole("alert").closest(".notice")!;
    const recovery = within(notice as HTMLElement);
    const retry = recovery.getByRole("button", {
      name: /повтор|провер|подключ/i,
    });
    expect(retry).toBeEnabled();
    await user.click(recovery.getByRole("button", { name: /сервер|подпис/i }));
    expect(screen.getByRole("heading", { name: "Серверы" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Frankfurt 01" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent(coreError);
    await user.click(
      within(
        screen.getByRole("alert").closest(".notice")! as HTMLElement,
      ).getByRole("button", { name: /журнал/i }),
    );
    expect(
      screen.getByRole("heading", { name: "Журнал и диагностика" }),
    ).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent(coreError);
    await user.click(retry);
    expect(execute).toHaveBeenCalledExactlyOnceWith({
      type: active ? "check_now" : "connect",
    });
  },
);

it("retains subscription input and the working connection after a save failure", async () => {
  const user = userEvent.setup();
  const { execute, backend } = mount(fixture, "servers");
  execute.mockRejectedValue("Не удалось сохранить подписку");
  await user.click(
    (
      await screen.findAllByRole("button", {
        name: "Добавить подписку",
      })
    )[0],
  );
  const dialog = screen.getByRole("dialog");
  await user.type(
    within(dialog).getByLabelText(/^(URL|Ссылка) подписки$/),
    "https://example.com/subscription",
  );
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Не удалось сохранить подписку",
  );
  expect(within(dialog).getByLabelText(/^(URL|Ссылка) подписки$/)).toHaveValue(
    "https://example.com/subscription",
  );
  expect(await backend.snapshot()).toMatchObject({
    selected: "a",
    runtime_active: true,
  });
});

it("shows backend appVersion in the footer without a brand version", async () => {
  const backend = new MockBackend();
  vi.spyOn(backend, "appVersion").mockResolvedValue("7.4.3");
  const { container } = render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  await waitFor(() =>
    expect(screen.getByRole("contentinfo")).toHaveTextContent("7.4.3"),
  );
  expect(container.querySelector(".brand")).toHaveTextContent("Rift");
  expect(container.querySelector(".brand")).not.toHaveTextContent(
    /7\.4\.3|0\.1\.0/,
  );
  expect(screen.getByRole("contentinfo")).not.toHaveTextContent(
    /Работает без сбора данных|Ошибки отправляются без личных данных/,
  );
});

it.each(["connection", "servers", "settings"] as const)(
  "never displays legacy country metadata on %s or rewrites the name",
  async (page) => {
    const legacy = {
      ...fixture.servers[0],
      name: "Frankfurt DE 01",
      country: "DE",
    };
    mount({ ...fixture, servers: [legacy] }, page);
    if (page === "connection")
      await screen.findByRole("region", { name: "Подключение" });
    else
      await screen.findByRole("heading", {
        name: page === "servers" ? "Серверы" : "Настройки",
      });
    if (page !== "settings")
      expect(screen.getByText("Frankfurt DE 01")).toBeVisible();
    expect(
      screen.queryByRole("columnheader", { name: "Страна" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(/Германия|Страна не указана|DB-IP|🇩🇪/),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("DE", { exact: true })).not.toBeInTheDocument();
  },
);

it("keeps diagnostic connection values available behind an explicit disclosure", async () => {
  const user = userEvent.setup();
  const { container } = mount();
  await screen.findByRole("region", { name: "Подключение" });
  const disclosure = [...container.querySelectorAll("summary, button")].find(
    (element) => /подроб|диагност/i.test(element.textContent ?? ""),
  );
  expect(disclosure).toBeDefined();
  const details = disclosure!.closest("details");
  if (details) expect(details).not.toHaveAttribute("open");
  await user.click(disclosure!);
  // The current values, apart from the measurements listed below them.
  const current = within(container.querySelector<HTMLElement>(".latencies")!);
  expect(current.getByText("31 мс", { exact: true })).toBeVisible();
  expect(current.getByText("64 мс", { exact: true })).toBeVisible();
  expect(screen.getByText("127.0.0.1:2080", { exact: true })).toBeVisible();
  expect(screen.getByText("Frankfurt 01", { exact: true })).toBeVisible();
});

it("says when a server that did not answer is asked again, and says nothing for the others", () => {
  const silent = {
    ...fixture.servers[1],
    status: "failed",
    retry_at: 1789743000,
  };
  render(
    <ServerTable
      servers={[silent, fixture.servers[3]]}
      selected={null}
      sources={fixture.subscriptions}
      sort={fixture.servers_sort}
      onSort={vi.fn()}
      onSelect={vi.fn()}
      onDelete={vi.fn()}
    />,
  );
  const rows = screen.getAllByRole("row");
  expect(
    within(rows[1]).getByText(/^Спросим снова в \d{2}:\d{2}$/),
  ).toBeInTheDocument();
  expect(within(rows[2]).queryByText(/Спросим снова/)).toBeNull();
  expect(screen.getAllByText(/Спросим снова/)).toHaveLength(1);
});

it("keeps source, protocol and last-check diagnostics available on the server list", async () => {
  const user = userEvent.setup();
  const { container } = mount(
    { ...fixture, servers: [fixture.servers[3]] },
    "servers",
  );
  await screen.findByRole("button", { name: "Warsaw 03" });
  const disclosures = [...container.querySelectorAll("summary, button")].filter(
    (element) => /подроб|диагност/i.test(element.textContent ?? ""),
  );
  expect(disclosures.length).toBeGreaterThan(0);
  for (const disclosure of disclosures) await user.click(disclosure);
  expect(screen.getByText("shadowsocks", { exact: true })).toBeVisible();
  expect(screen.getByText("52 мс", { exact: true })).toBeVisible();
  expect(screen.getByText("74 мс", { exact: true })).toBeVisible();
  const row = screen.getByRole("button", { name: "Warsaw 03" }).closest("tr")!;
  expect(
    within(row).getByText(checkedAt(fixture.servers[3].checked_at), {
      exact: true,
    }),
  ).toBeVisible();
  expect(within(row).getByText("Вручную", { exact: true })).toBeVisible();
});

it("shows the connection page without its title or the explanatory lines", async () => {
  const { container } = mount();
  const page = await screen.findByRole("region", { name: "Подключение" });
  expect(
    within(page).queryByRole("heading", { name: "Подключение" }),
  ).not.toBeInTheDocument();
  for (const text of [
    "Соединение через выбранный сервер",
    /Локальный прокси для ваших приложений/,
    /Системный трафик через TUN/,
    /Rift проверяет соединение и выбирает рабочий сервер/,
  ])
    expect(screen.queryByText(text)).not.toBeInTheDocument();
  // The state itself stays; its empty subtitle takes no room.
  expect(
    await screen.findByRole("heading", { name: "Подключено" }),
  ).toBeVisible();
  expect(container.querySelector(".connection-status p")).toBeEmptyDOMElement();
});

it("keeps the mode and the local proxy visible on the connection page, outside the disclosure", async () => {
  mount();
  await screen.findByRole("region", { name: "Подключение" });
  for (const label of ["Режим работы", "Локальный прокси"]) {
    const fact = screen.getByText(label, { exact: true });
    expect(fact).toBeVisible();
    expect(fact.closest("details")).toBeNull();
  }
  expect(screen.getByText("127.0.0.1:2080", { exact: true })).toBeVisible();
  const details = screen
    .getByText("Подробности соединения")
    .closest("details")!;
  expect(within(details).getByText("Протокол")).toBeInTheDocument();
  expect(within(details).queryByText("Режим работы")).not.toBeInTheDocument();
});

it.each([true, false])(
  "puts «Проверить всё» under «Обновить все» (active=%s) and no check button in the page header",
  async (active) => {
    const user = userEvent.setup();
    const { execute } = mount(
      active ? fixture : { ...fixture, runtime_active: false },
      "servers",
    );
    await screen.findByRole("heading", { name: "Серверы" });
    expect(
      screen.queryByRole("button", { name: "Проверить сейчас" }),
    ).not.toBeInTheDocument();
    const refresh = screen.getByRole("button", { name: "Обновить все" });
    const check = screen.getByRole("button", { name: "Проверить всё" });
    expect(refresh.nextElementSibling).toBe(check);
    expect(check.className).toBe(refresh.className);
    expect(check.closest(".page-head")).toBeNull();
    if (!active) {
      expect(check).toBeDisabled();
      return;
    }
    await user.click(check);
    await waitFor(() =>
      expect(execute).toHaveBeenCalledExactlyOnceWith({ type: "check_now" }),
    );
  },
);

it("lists subscriptions with the count on the right and the update time after the buttons", async () => {
  mount(fixture, "servers");
  await screen.findByRole("heading", { name: "Серверы" });
  const heading = screen.getByRole("heading", { name: "Подписки" });
  expect(heading.parentElement).toHaveTextContent(/^Подписки$/);
  const backup = screen
    .getByRole("button", { name: /^Резервная/ })
    .closest(".subscription") as HTMLElement;
  // «Резервная 1», not «Резервная 1 сервер»: the word is gone, the number is a count.
  expect(within(backup).queryByText(/сервер/)).not.toBeInTheDocument();
  expect(
    backup.querySelector(".subscription-title .source-count"),
  ).toHaveTextContent(/^1$/);
  const names = [...backup.querySelectorAll(".subscription-actions > *")].map(
    (element) => element.getAttribute("aria-label") ?? element.className,
  );
  expect(names).toEqual([
    "Изменить Резервная",
    "Удалить Резервная",
    "Обновить Резервная",
    "subscription-updated",
  ]);
  const updated = backup.querySelector(".subscription-updated")!;
  expect(updated).toHaveTextContent(time(fixture.subscriptions[1].updated_at));
  expect(backup).not.toHaveTextContent("Обновление");
  // «Все серверы» and «Вручную» use the same count element.
  for (const name of [/^Все серверы/, /^Вручную/])
    expect(
      screen.getByRole("button", { name }).querySelector(".source-count"),
    ).not.toBeNull();
});

it("keeps a subscription failure as its own line and says when it was never updated", async () => {
  mount(
    {
      ...fixture,
      subscriptions: [
        {
          ...fixture.subscriptions[0],
          updated_at: null,
          error: "Сеть недоступна",
        },
      ],
    },
    "servers",
  );
  await screen.findByRole("heading", { name: "Серверы" });
  const item = screen
    .getByRole("button", { name: /^Основная/ })
    .closest(".subscription") as HTMLElement;
  expect(within(item).getByText("Сеть недоступна")).toBeVisible();
  expect(item.querySelector(".subscription-updated")).toHaveTextContent(
    "Не обновлялась",
  );
});

it("does not repeat the journal promise in the subscription dialog", async () => {
  const user = userEvent.setup();
  mount(fixture, "servers");
  await user.click(
    (await screen.findAllByRole("button", { name: "Добавить подписку" }))[0],
  );
  const dialog = screen.getByRole("dialog");
  expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveFocus();
  expect(
    within(dialog).queryByText(/не попадает в журнал/),
  ).not.toBeInTheDocument();
});

it("shows a reported error as a toast, but not next to a banner that says the same", async () => {
  const { backend } = mount({
    ...fixture,
    status: "error",
    error: "Нет доступных серверов",
  });
  await screen.findByRole("region", { name: "Подключение" });
  act(() => backend.raiseError("Нет доступных серверов"));
  expect(
    screen.queryByRole("button", { name: "Скрыть ошибку" }),
  ).not.toBeInTheDocument();
  act(() => backend.raiseError("Не удалось обновить подписку «Основная»"));
  const hide = await screen.findByRole("button", { name: "Скрыть ошибку" });
  expect(hide.closest(".toast")).toHaveTextContent("Не удалось обновить");
  // There is no second window to open or close.
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

// ---- a subscription that gave no servers ------------------------------------------------------

const noServers = (
  change: Partial<Snapshot["subscriptions"][number]>,
): Snapshot => ({
  ...fixture,
  servers: [],
  selected: null,
  status: "disconnected",
  runtime_active: false,
  subscriptions: [
    {
      ...fixture.subscriptions[0],
      id: "main",
      name: "Основная",
      count: 0,
      ...change,
    },
  ],
});

it("says why a subscription gave no servers and offers to ask again, not to add a server", async () => {
  const user = userEvent.setup();
  const { execute } = mount(
    noServers({ failure_code: "subscription_transport" as never, error: "x" }),
    "servers",
  );
  await user.click(await screen.findByRole("button", { name: /^Основная/ }));
  const heading = await screen.findByRole("heading", {
    name: "Не удалось получить серверы подписки «Основная»",
  });
  const empty = heading.closest(".empty") as HTMLElement;
  expect(
    within(empty).getByText("Не удалось загрузить подписку"),
  ).toBeVisible();
  expect(
    within(empty).queryByRole("button", { name: "Добавить сервер" }),
  ).toBeNull();
  await user.click(within(empty).getByRole("button", { name: "Обновить" }));
  expect(execute).toHaveBeenCalledWith({ type: "refresh", id: "main" });
});

it("says every subscription failed when the whole list is empty", async () => {
  const user = userEvent.setup();
  const { execute } = mount(
    noServers({ failure_code: "subscription_transport" as never, error: "x" }),
    "servers",
  );
  const heading = await screen.findByRole("heading", {
    name: "Серверов пока нет: подписки не загрузились",
  });
  await user.click(
    within(heading.closest(".empty") as HTMLElement).getByRole("button", {
      name: "Обновить все",
    }),
  );
  expect(execute).toHaveBeenCalledWith({ type: "refresh", id: null });
});

it("says the filters hid everything and opens the subscription to change them", async () => {
  const user = userEvent.setup();
  mount(
    noServers({ failure_code: "subscription_filtered" as never }),
    "servers",
  );
  await user.click(await screen.findByRole("button", { name: /^Основная/ }));
  expect(
    await screen.findByRole("heading", {
      name: "Фильтры скрыли все серверы подписки",
    }),
  ).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Изменить подписку" }));
  expect(await screen.findByRole("dialog")).toBeVisible();
});

it("keeps the old words for a list with nothing in it", async () => {
  mount(
    { ...fixture, servers: [], selected: null, subscriptions: [] },
    "servers",
  );
  const heading = await screen.findByRole("heading", {
    name: "Серверов пока нет",
  });
  expect(
    screen.getByText("Добавьте ссылку подписки или сервер вручную."),
  ).toBeVisible();
  expect(
    within(heading.closest(".empty") as HTMLElement).getByRole("button", {
      name: "Добавить подписку",
    }),
  ).toBeVisible();
});

it("keeps the old words for the manual list when the servers are elsewhere", async () => {
  const user = userEvent.setup();
  mount(
    { ...fixture, servers: fixture.servers.filter((s) => s.source !== null) },
    "servers",
  );
  await user.click(await screen.findByRole("button", { name: /^Вручную/ }));
  expect(
    await screen.findByRole("heading", {
      name: "В этом источнике пока нет серверов",
    }),
  ).toBeVisible();
  expect(
    screen.getByText("Выберите другой источник слева или добавьте сервер"),
  ).toBeVisible();
});

it("does not tell to add a server on the connection page while there are subscriptions", async () => {
  mount(
    noServers({ failure_code: "subscription_transport" as never }),
    "connection",
  );
  expect(
    await screen.findByText(
      "Подписка пока не дала серверов. Проверьте раздел «Серверы».",
    ),
  ).toBeVisible();
  expect(screen.getByText("Подписка пока не дала серверов")).toBeVisible();
  expect(screen.queryByText(/Добавьте сервер в разделе/)).toBeNull();
});

it("tells to add a server on the connection page when there is nothing at all", async () => {
  mount(
    {
      ...fixture,
      servers: [],
      selected: null,
      subscriptions: [],
      status: "disconnected",
    },
    "connection",
  );
  expect(
    await screen.findByText(/Добавьте сервер в разделе «Серверы»/),
  ).toBeVisible();
});
