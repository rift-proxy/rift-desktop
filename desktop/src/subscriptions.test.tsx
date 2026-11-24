import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { InputPreview, Snapshot, Subscription } from "./backend/types";
import { MockBackend, fixture, noOptions } from "./testing/mock";

const mount = (state: Snapshot = fixture) => {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  return { backend, execute };
};
const withSubscription = (change: Partial<Subscription>): Snapshot => ({
  ...fixture,
  subscriptions: [
    { ...fixture.subscriptions[0], ...change },
    fixture.subscriptions[1],
  ],
});
const saved = (execute: { mock: { calls: unknown[][] } }) =>
  execute.mock.calls
    .map(([action]) => action as { type: string })
    .filter((action) => action.type === "save_subscription");

it("keeps sources always active without source switches, including the subscription form", async () => {
  const user = userEvent.setup();
  mount();
  await screen.findByRole("button", { name: "Основная 2" });
  expect(screen.queryByRole("switch", { name: "Включена" })).toBeNull();
  await user.click(screen.getByRole("button", { name: "Изменить Основная" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).queryByRole("switch", { name: "Включена" })).toBeNull();
  await user.click(within(dialog).getByText("Дополнительно"));
  expect(
    within(dialog).getByRole("switch", {
      name: "Применять маршруты провайдера",
    }),
  ).toBeInTheDocument();
});

it("sends the options of a new subscription and never a name", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await user.click(
    (await screen.findAllByRole("button", { name: "Добавить подписку" }))[0],
  );
  const dialog = screen.getByRole("dialog");
  expect(within(dialog).queryByLabelText("Название")).not.toBeInTheDocument();
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки"),
    "https://panel.example/sub",
  );
  await user.click(within(dialog).getByText("Дополнительно"));
  await user.type(
    within(dialog).getByLabelText("Интервал обновления, мин"),
    "15",
  );
  await user.type(within(dialog).getByLabelText("User-Agent"), "Happ/1.0");
  await user.type(
    within(dialog).getByLabelText("Оставить серверы с названием"),
    "DE, nl ,, ",
  );
  await user.type(
    within(dialog).getByLabelText("Скрыть серверы с названием"),
    "test",
  );
  await user.click(within(dialog).getByRole("checkbox", { name: "VLESS" }));
  await user.click(within(dialog).getByRole("checkbox", { name: "Trojan" }));
  await user.click(within(dialog).getByRole("checkbox", { name: "Trojan" }));
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)).toEqual([
    {
      type: "save_subscription",
      id: null,
      url: "https://panel.example/sub",
      interval_mins: 15,
      user_agent: "Happ/1.0",
      filters: {
        include: ["DE", "nl"],
        exclude: ["test"],
        protocols: ["vless"],
      },
      provider_routes: true,
    },
  ]);
});

it("leaves the options empty when none is set: the global ones apply", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await user.click(
    (await screen.findAllByRole("button", { name: "Добавить подписку" }))[0],
  );
  const dialog = screen.getByRole("dialog");
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки"),
    "https://panel.example/sub",
  );
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)).toEqual([
    {
      type: "save_subscription",
      id: null,
      url: "https://panel.example/sub",
      interval_mins: null,
      user_agent: null,
      filters: { include: [], exclude: [], protocols: [] },
      provider_routes: true,
    },
  ]);
});

it("keeps the rules of the provider unless the switch is turned off", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await user.click(
    (await screen.findAllByRole("button", { name: "Добавить подписку" }))[0],
  );
  const dialog = screen.getByRole("dialog");
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки"),
    "https://panel.example/sub",
  );
  await user.click(within(dialog).getByText("Дополнительно"));
  const routes = within(dialog).getByRole("switch", {
    name: "Применять маршруты провайдера",
  });
  expect(routes).toBeChecked();
  await user.click(routes);
  expect(routes).not.toBeChecked();
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)[0]).toMatchObject({ provider_routes: false });
});

it("opens the options of a subscription whose switch is off, and shows it off", async () => {
  const user = userEvent.setup();
  const { execute } = mount(withSubscription({ provider_routes: false }));
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  // Open at once, like for any option that is set.
  const routes = within(dialog).getByRole("switch", {
    name: "Применять маршруты провайдера",
  });
  expect(routes).toBeVisible();
  expect(routes).not.toBeChecked();
  // The address is the one the subscription has: saving without touching it is enough.
  await waitFor(() =>
    expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveValue(
      "https://panel.example/main",
    ),
  );
  await user.click(within(dialog).getByRole("button", { name: "Сохранить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)[0]).toMatchObject({
    id: "main",
    provider_routes: false,
    url: "https://panel.example/main",
  });
});

it("shows the stored options when a subscription is edited and keeps its name", async () => {
  const user = userEvent.setup();
  const { execute } = mount(
    withSubscription({
      interval_mins: 30,
      user_agent: "v2rayN/7",
      filters: {
        include: ["DE"],
        exclude: ["slow", "test"],
        protocols: ["trojan"],
      },
    }),
  );
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  // The options are open at once because the subscription has some.
  expect(within(dialog).getByLabelText("Интервал обновления, мин")).toHaveValue(
    30,
  );
  expect(within(dialog).getByLabelText("User-Agent")).toHaveValue("v2rayN/7");
  expect(
    within(dialog).getByLabelText("Оставить серверы с названием"),
  ).toHaveValue("DE");
  expect(
    within(dialog).getByLabelText("Скрыть серверы с названием"),
  ).toHaveValue("slow, test");
  expect(
    within(dialog).getByRole("checkbox", { name: "Trojan" }),
  ).toBeChecked();
  expect(
    within(dialog).getByRole("checkbox", { name: "VLESS" }),
  ).not.toBeChecked();
  const address = within(dialog).getByLabelText("Ссылка подписки");
  await waitFor(() =>
    expect(address).toHaveValue("https://panel.example/main"),
  );
  await user.clear(address);
  await user.type(address, "https://panel.example/new");
  await user.click(within(dialog).getByRole("button", { name: "Сохранить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)[0]).toMatchObject({
    id: "main",
    interval_mins: 30,
    url: "https://panel.example/new",
  });
  expect(saved(execute)[0]).not.toHaveProperty("name");
  expect(screen.getByRole("button", { name: /^Основная \d+$/ })).toBeVisible();
});

it("shows the traffic, the term and the last update of a subscription", async () => {
  const term = new Date(2027, 0, 15, 12, 0, 0).getTime() / 1000;
  mount(
    withSubscription({
      usage: { used: 40 * 1024 ** 2, total: 100 * 1024 ** 3, expire: term },
    }),
  );
  await screen.findByRole("button", { name: /^Основная/ });
  expect(
    screen.getByText("40 МБ из 100 ГБ · Действует до 15.01.2027"),
  ).toBeVisible();
  expect(screen.queryByText(/не участвует в автовыборе/)).toBeNull();
});

it.each([
  ["exhausted", "Трафик исчерпан: не участвует в автовыборе"],
  ["expired", "Срок истёк: не участвует в автовыборе"],
] as const)(
  "marks a %s subscription as left out of Auto Select",
  async (availability, note) => {
    const term = new Date(2025, 0, 15, 12, 0, 0).getTime() / 1000;
    mount(
      withSubscription({
        availability,
        usage: {
          used: 100 * 1024 ** 3,
          total: 100 * 1024 ** 3,
          expire: availability === "expired" ? term : null,
        },
      }),
    );
    expect(await screen.findByText(note)).toBeVisible();
    if (availability === "expired")
      expect(screen.getByText(/Срок истёк 15\.01\.2025/)).toBeVisible();
    // Its servers stay in the list: they can still be chosen by hand.
    expect(screen.getByRole("button", { name: "Frankfurt 01" })).toBeVisible();
  },
);

it("shows nothing about traffic when the provider reports none", async () => {
  mount({
    ...fixture,
    subscriptions: fixture.subscriptions.map((s) => ({ ...s, ...noOptions })),
  });
  await screen.findByRole("button", { name: /^Основная/ });
  expect(screen.queryByText(/Действует до|Использовано|из \d/)).toBeNull();
});

// ---- Ctrl+V on the page of servers --------------------------------------------------------

const paste = (text: string, target: Element = document.body) =>
  fireEvent.paste(target, { clipboardData: { getData: () => text } });
const added = (execute: { mock: { calls: unknown[][] } }) =>
  execute.mock.calls
    .map(([action]) => action as { type: string })
    .filter((action) => /^add_|^save_subscription$/.test(action.type));

it("takes a pasted subscription link and offers it in the subscription dialog", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await screen.findByRole("heading", { name: "Серверы" });
  paste("Моя подписка: https://panel.example/sub?token=abc, спасибо");
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("Добавить подписку")).toBeVisible();
  expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveValue(
    "https://panel.example/sub?token=abc",
  );
  expect(added(execute)).toEqual([]);
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(added(execute)).toMatchObject([
    { type: "save_subscription", url: "https://panel.example/sub?token=abc" },
  ]);
});

it("takes one pasted server link and adds it only after the confirmation", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await screen.findByRole("heading", { name: "Серверы" });
  paste("vless://11111111-1111-4111-8111-111111111111@a.example:443#A");
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("Добавить серверы")).toBeVisible();
  expect(within(dialog).getByText("Новый сервер 1")).toBeVisible();
  expect(added(execute)).toEqual([]);
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(added(execute)).toEqual([
    {
      type: "add_servers",
      uris: ["vless://11111111-1111-4111-8111-111111111111@a.example:443#A"],
    },
  ]);
});

it("adds several pasted servers after one confirmation", async () => {
  const user = userEvent.setup();
  const { execute, backend } = mount();
  await screen.findByRole("heading", { name: "Серверы" });
  const links = Array.from(
    { length: 8 },
    (_, i) => `vless://u@h${i}.example:443`,
  );
  paste(links.join("\n"));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("Найдено серверов: 8")).toBeVisible();
  expect(within(dialog).getByText("и ещё 2")).toBeVisible();
  await user.click(within(dialog).getByRole("button", { name: "Добавить 8" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(added(execute)).toEqual([{ type: "add_servers", uris: links }]);
  expect((await backend.snapshot()).servers).toHaveLength(
    fixture.servers.length + 8,
  );
});

it("leaves a canceled paste unadded", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await screen.findByRole("heading", { name: "Серверы" });
  paste("vless://u@a.example:443");
  const dialog = await screen.findByRole("dialog");
  await user.click(within(dialog).getByRole("button", { name: "Отмена" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(added(execute)).toEqual([]);
});

it("ignores a paste without a link, a paste into a field and a paste on another page", async () => {
  const user = userEvent.setup();
  mount();
  await screen.findByRole("heading", { name: "Серверы" });
  paste("просто текст без ссылок");
  paste("   ");
  expect(screen.queryByRole("dialog")).toBeNull();

  // Into a field the paste is the field's own: no second dialog on top of the first.
  await user.click(screen.getByRole("button", { name: "Добавить сервер" }));
  const dialog = await screen.findByRole("dialog");
  paste(
    "https://panel.example/sub",
    within(dialog).getByLabelText("Ссылка сервера"),
  );
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect(within(dialog).getByText("Добавить сервер")).toBeVisible();
});

it("does not take a paste outside the page of servers", async () => {
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await screen.findByRole("heading", { name: "Настройки" });
  paste("https://panel.example/sub");
  expect(screen.queryByRole("dialog")).toBeNull();
});

// ---- QR codes and rift:// links ---------------------------------------------------------

const subscriptionLink: InputPreview = {
  kind: "subscription",
  url: "https://panel.example/sub?token=abc",
};
const oneServer: InputPreview = {
  kind: "servers",
  rejected: 0,
  items: [
    {
      uri: "vless://u@a.example:443#A",
      server: { ...fixture.servers[0], id: "qr-1", name: "Из QR" },
    },
  ],
};

it("opens the subscription dialog with the link of a QR code from a file", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.qr = subscriptionLink;
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await user.click(await screen.findByRole("button", { name: "QR из файла" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveValue(
    "https://panel.example/sub?token=abc",
  );
  expect(added(execute)).toEqual([]);
});

it("shows the servers of a QR code from the clipboard and adds them only on confirm", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.qr = oneServer;
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await user.click(await screen.findByRole("button", { name: "QR из буфера" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("Из QR")).toBeVisible();
  expect(added(execute)).toEqual([]);
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(added(execute)).toEqual([
    { type: "add_servers", uris: ["vless://u@a.example:443#A"] },
  ]);
});

it("does nothing when the file dialog is canceled or the picture has no code", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  const file = await screen.findByRole("button", { name: "QR из файла" });
  backend.qr = null; // the dialog was canceled
  await user.click(file);
  backend.qr = undefined; // a picture without a code: the host reports it, the page stays calm
  await user.click(file);
  await user.click(screen.getByRole("button", { name: "QR из буфера" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("heading", { name: "Серверы" })).toBeVisible();
});

it("opens the dialog of a rift:// link from any page", async () => {
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await screen.findByRole("heading", { name: "Настройки" });
  await waitFor(() => expect(backend["importListeners"].size).toBe(1));
  act(() => backend.pushImport(subscriptionLink));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveValue(
    "https://panel.example/sub?token=abc",
  );
  // The page behind the dialog is the one of servers now.
  expect(screen.getByRole("heading", { name: "Серверы" })).toBeInTheDocument();
});

it("stops listening for rift:// links when the app goes away", async () => {
  const backend = new MockBackend();
  const view = render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await waitFor(() => expect(backend["importListeners"].size).toBe(1));
  view.unmount();
  expect(backend["importListeners"].size).toBe(0);
});

it("shows the rift:// link that started Rift once the page is ready", async () => {
  const backend = new MockBackend();
  backend.pendingImport = subscriptionLink;
  render(
    <BackendProvider client={backend}>
      <App initialPage="connection" />
    </BackendProvider>,
  );
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByLabelText("Ссылка подписки")).toHaveValue(
    "https://panel.example/sub?token=abc",
  );
  expect(screen.getByRole("heading", { name: "Серверы" })).toBeInTheDocument();
  // It is shown once: the next page load finds nothing waiting.
  expect(await backend.takeImport()).toBeNull();
});

it("replaces an open subscription dialog when a newer incoming link arrives", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  await waitFor(() => expect(backend["importListeners"].size).toBe(1));
  act(() => backend.pushImport(subscriptionLink));
  await screen.findByRole("dialog");
  const newer = { ...subscriptionLink, url: "https://newer.example/sub" };
  act(() => backend.pushImport(newer));
  expect(screen.getByLabelText("Ссылка подписки")).toHaveValue(newer.url);
  await user.click(
    within(screen.getByRole("dialog")).getByRole("button", {
      name: "Добавить",
    }),
  );
  await waitFor(() =>
    expect(execute).toHaveBeenCalledWith(
      expect.objectContaining({ type: "save_subscription", url: newer.url }),
    ),
  );
});

it("shows the address of the subscription that is edited, and saves it unchanged when it is not touched", async () => {
  const user = userEvent.setup();
  const { backend, execute } = mount();
  backend.subscriptionUrls.main = "https://panel.example/main?token=abc";
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  const address = within(dialog).getByLabelText("Ссылка подписки");
  await waitFor(() =>
    expect(address).toHaveValue("https://panel.example/main?token=abc"),
  );
  expect(address).toBeEnabled();
  await user.click(within(dialog).getByRole("button", { name: "Сохранить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(saved(execute)[0]).toMatchObject({
    id: "main",
    url: "https://panel.example/main?token=abc",
  });
});

it("holds the field while the address is read, and does not take the empty field for an address", async () => {
  const user = userEvent.setup();
  const { backend, execute } = mount();
  let answer: (url: string) => void = () => {};
  backend.subscriptionUrl = () =>
    new Promise((resolve) => {
      answer = resolve;
    });
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  const address = within(dialog).getByLabelText("Ссылка подписки");
  expect(address).toBeDisabled();
  expect(address).toHaveAttribute("placeholder", "Читаем адрес…");
  // Nothing is sent while the address is not known.
  await user.click(within(dialog).getByRole("button", { name: "Сохранить" }));
  expect(saved(execute)).toHaveLength(0);
  await act(async () => answer("https://panel.example/late"));
  await waitFor(() => expect(address).toBeEnabled());
  expect(address).toHaveValue("https://panel.example/late");
});

it("puts the focus in the address field when the address has been read, not on the close icon", async () => {
  const user = userEvent.setup();
  const { backend } = mount();
  let answer: (url: string) => void = () => {};
  backend.subscriptionUrl = () =>
    new Promise((resolve) => {
      answer = resolve;
    });
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  const address = within(dialog).getByLabelText("Ссылка подписки");
  // The field is held, so the focus the dialog tried to give it stayed on the way out.
  expect(address).toBeDisabled();
  expect(address).not.toHaveFocus();
  await act(async () => answer("https://panel.example/late"));
  await waitFor(() => expect(address).toBeEnabled());
  expect(address).toHaveFocus();
});

it("leaves the focus where the user has put it while the address is read", async () => {
  const user = userEvent.setup();
  const { backend } = mount();
  let answer: (url: string) => void = () => {};
  backend.subscriptionUrl = () =>
    new Promise((resolve) => {
      answer = resolve;
    });
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  const cancel = within(dialog).getByRole("button", { name: "Отмена" });
  act(() => cancel.focus());
  await act(async () => answer("https://panel.example/late"));
  const address = within(dialog).getByLabelText("Ссылка подписки");
  await waitFor(() => expect(address).toBeEnabled());
  expect(cancel).toHaveFocus();
});

it("puts the focus in the empty field when the address could not be read", async () => {
  const user = userEvent.setup();
  const { backend } = mount();
  backend.subscriptionUrl = () => Promise.reject("Подписка не найдена");
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  await within(dialog).findByText("Не удалось прочитать адрес подписки");
  const address = within(dialog).getByLabelText("Ссылка подписки");
  expect(address).toHaveValue("");
  await waitFor(() => expect(address).toHaveFocus());
});

it("says the address could not be read, and keeps the subscription as it is", async () => {
  const user = userEvent.setup();
  const { backend, execute } = mount();
  backend.subscriptionUrl = () => Promise.reject("Подписка не найдена");
  await user.click(
    await screen.findByRole("button", { name: "Изменить Основная" }),
  );
  const dialog = screen.getByRole("dialog");
  expect(
    await within(dialog).findByText("Не удалось прочитать адрес подписки"),
  ).toBeVisible();
  const address = within(dialog).getByLabelText("Ссылка подписки");
  expect(address).toHaveValue("");
  expect(address).toBeEnabled();
  // The field is required: nothing empty replaces the address of the subscription.
  await user.click(within(dialog).getByRole("button", { name: "Сохранить" }));
  expect(saved(execute)).toHaveLength(0);
  await user.click(within(dialog).getByRole("button", { name: "Отмена" }));
  expect(screen.queryByRole("dialog")).toBeNull();
});
