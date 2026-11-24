import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { ConnectionRow, Connections } from "./backend/types";
import { MockBackend, fixture } from "./testing/mock";

const row = (changes: Partial<ConnectionRow> = {}): ConnectionRow => ({
  host: "example.com",
  port: 443,
  network: "tcp",
  route: "proxy",
  server: "Frankfurt 01",
  process: null,
  upload: 2048,
  download: 3 * 1024 ** 2,
  seconds: 130,
  ...changes,
});
const list = (items: ConnectionRow[], total = items.length): Connections => ({
  total,
  items,
});
const mount = async (backend: MockBackend) => {
  await act(async () => {
    render(
      <BackendProvider client={backend}>
        <App initialPage="connections" />
      </BackendProvider>,
    );
  });
};
/** The rows of the table, as the text of their cells. */
const cells = () =>
  screen
    .getAllByRole("row")
    .slice(1)
    .map((tr) =>
      within(tr)
        .getAllByRole("cell")
        .map((cell) => cell.textContent),
    );

it("asks for nothing and says why while Rift is not connected", async () => {
  const backend = new MockBackend({
    ...fixture,
    status: "disconnected",
    runtime_active: false,
  });
  await mount(backend);
  expect(
    await screen.findByRole("heading", { name: "Нет подключения" }),
  ).toBeVisible();
  await new Promise((resolve) => setTimeout(resolve, 30));
  expect(backend.watchingConnections).toEqual([]);
});

it("waits for the first list, then shows each connection with what it needs", async () => {
  const backend = new MockBackend();
  await mount(backend);
  expect(await screen.findByText("Читаем соединения…")).toBeVisible();
  await waitFor(() => expect(backend.watchingConnections).toEqual([true]));

  act(() =>
    backend.pushConnections(
      list([
        row({ process: "chrome.exe" }),
        row({
          host: "192.0.2.7",
          port: 53,
          network: "udp",
          route: "direct",
          server: null,
          seconds: 7,
          download: 0,
          upload: 0,
        }),
        row({
          host: "",
          port: 0,
          network: "other",
          route: "other",
          server: null,
          seconds: null,
        }),
      ]),
    ),
  );
  expect(await screen.findByRole("table")).toBeVisible();
  expect(cells()).toEqual([
    [
      "example.com:443",
      "TCP",
      "Frankfurt 01",
      "chrome.exe",
      "3 МБ",
      "2 КБ",
      "2 мин 10 с",
    ],
    ["192.0.2.7:53", "UDP", "Напрямую", "—", "0 Б", "0 Б", "7 с"],
    ["—", "—", "—", "—", "3 МБ", "2 КБ", "—"],
  ]);
  // The columns of volume have a name for a screen reader.
  expect(screen.getByText("Получено")).toBeInTheDocument();
  expect(screen.getByText("Отправлено")).toBeInTheDocument();
});

it("says how to get a program name when no row has one", async () => {
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  act(() => backend.pushConnections(list([row(), row({ host: "b.example" })])));
  expect(
    await screen.findByText(/Имя программы не определяется/),
  ).toBeVisible();
  act(() => backend.pushConnections(list([row({ process: "app.exe" })])));
  await waitFor(() =>
    expect(screen.queryByText(/Имя программы не определяется/)).toBeNull(),
  );
});

it("says how many connections there are when the list is cut", async () => {
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  act(() => backend.pushConnections(list([row(), row()], 812)));
  expect(await screen.findByText("Показаны новейшие 2 из 812.")).toBeVisible();
  act(() => backend.pushConnections(list([row(), row()])));
  await waitFor(() =>
    expect(screen.queryByText(/Показаны новейшие/)).toBeNull(),
  );
});

it("filters by host, server, program and protocol", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  act(() =>
    backend.pushConnections(
      list([
        row({ host: "alpha.example", process: "Browser.exe" }),
        row({ host: "beta.example", server: "Helsinki 02" }),
        row({
          host: "gamma.example",
          network: "udp",
          route: "direct",
          server: null,
        }),
      ]),
    ),
  );
  const search = await screen.findByLabelText("Поиск по соединениям");
  for (const [text, hosts] of [
    ["ALPHA", ["alpha.example:443"]],
    ["helsinki", ["beta.example:443"]],
    ["browser", ["alpha.example:443"]],
    ["udp", ["gamma.example:443"]],
    ["напрямую", ["gamma.example:443"]],
    ["", ["alpha.example:443", "beta.example:443", "gamma.example:443"]],
  ] as const) {
    await user.clear(search);
    if (text) await user.type(search, text);
    expect(cells().map((c) => c[0])).toEqual(hosts);
  }
  await user.type(search, "no such connection");
  expect(
    await screen.findByRole("heading", { name: "Ничего не найдено" }),
  ).toBeVisible();
});

it("says there are none when the list is empty", async () => {
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  expect(backend.watchingConnections).toEqual([true]);
  act(() => backend.pushConnections(list([])));
  expect(
    await screen.findByRole("heading", { name: "Активных соединений нет" }),
  ).toBeVisible();
});

it("stops asking when the page is left, and starts from a clean page when it returns", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  await waitFor(() => expect(backend.watchingConnections).toEqual([true]));
  act(() => backend.pushConnections(list([row({ host: "stale.example" })])));
  await screen.findByText("stale.example", { exact: false });

  await user.click(screen.getByRole("button", { name: "Серверы" }));
  await waitFor(() =>
    expect(backend.watchingConnections).toEqual([true, false]),
  );
  await user.click(screen.getByRole("button", { name: "Соединения" }));
  await waitFor(() =>
    expect(backend.watchingConnections).toEqual([true, false, true]),
  );
  // What was on the page before is not shown again.
  expect(await screen.findByText("Читаем соединения…")).toBeVisible();
  expect(screen.queryByText("stale.example", { exact: false })).toBeNull();
});

it("goes back to waiting when the core stops reading, and keeps nothing in the browser", async () => {
  const backend = new MockBackend();
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  act(() => backend.pushConnections(list([row({ host: "secret.example" })])));
  await screen.findByText("secret.example", { exact: false });
  act(() => backend.pushConnections(null));
  expect(await screen.findByText("Читаем соединения…")).toBeVisible();
  for (const store of [localStorage, sessionStorage]) {
    for (let i = 0; i < store.length; i += 1) {
      expect(store.getItem(store.key(i)!)).not.toContain("secret.example");
    }
  }
});

it("is a page of the navigation", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App />
    </BackendProvider>,
  );
  await user.click(await screen.findByRole("button", { name: "Соединения" }));
  expect(
    await screen.findByRole("heading", { name: "Соединения", level: 1 }),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Соединения" })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

// ---- ordering ----------------------------------------------------------------------------------

const sample = () =>
  list([
    row({
      host: "b.example",
      port: 443,
      network: "udp",
      route: "direct",
      server: null,
      process: "zoom.exe",
      download: 500,
      upload: 30,
      seconds: 40,
    }),
    row({
      host: "a.example",
      port: 80,
      network: "tcp",
      route: "proxy",
      server: "Helsinki",
      process: null,
      download: 9000,
      upload: 10,
      seconds: null,
    }),
    row({
      host: "c.example",
      port: 22,
      network: "tcp",
      route: "proxy",
      server: "Amsterdam",
      process: "ssh.exe",
      download: 100,
      upload: 90,
      seconds: 3600,
    }),
  ]);
const hosts = () => cells().map((c) => c[0]?.split(":")[0]);
const header = (name: string) =>
  screen.getByRole("columnheader", {
    name: (accessible) => accessible.startsWith(name),
  });
const press = async (user: ReturnType<typeof userEvent.setup>, name: string) =>
  user.click(within(header(name)).getByRole("button"));
async function shown(backend: MockBackend, items = sample()) {
  await mount(backend);
  await screen.findByText("Читаем соединения…");
  act(() => backend.pushConnections(items));
  await screen.findByRole("table");
}

it.each([
  ["Хост", ["a.example", "b.example", "c.example"]],
  ["Протокол", ["a.example", "c.example", "b.example"]], // TCP, TCP, UDP: ties keep the order given
  ["Маршрут", ["b.example", "c.example", "a.example"]], // Напрямую, Amsterdam, Helsinki: the Russian collation puts Cyrillic first
  ["Программа", ["c.example", "b.example", "a.example"]], // ssh, zoom, none last
  ["Получено", ["c.example", "b.example", "a.example"]],
  ["Отправлено", ["a.example", "b.example", "c.example"]],
  ["Длительность", ["b.example", "c.example", "a.example"]], // 40 s, 1 h, none last
] as const)(
  "orders by %s on the first click, the other way on the second, and back on the third",
  async (name, upwards) => {
    const user = userEvent.setup();
    await shown(new MockBackend());
    expect(hosts()).toEqual(["b.example", "a.example", "c.example"]);
    await press(user, name);
    expect(header(name)).toHaveAttribute("aria-sort", "ascending");
    expect(hosts()).toEqual(upwards);
    await press(user, name);
    expect(header(name)).toHaveAttribute("aria-sort", "descending");
    const down = hosts();
    // Nothing without a value goes first, in either direction.
    if (name === "Программа") expect(down.at(-1)).toBe("a.example");
    if (name === "Длительность") expect(down.at(-1)).toBe("a.example");
    await press(user, name);
    expect(header(name)).not.toHaveAttribute("aria-sort");
    expect(hosts()).toEqual(["b.example", "a.example", "c.example"]);
  },
);

it("keeps the order across the refresh of the list, and orders a new row among the rest", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  await shown(backend);
  await press(user, "Получено");
  await press(user, "Получено");
  expect(hosts()).toEqual(["a.example", "b.example", "c.example"]);
  act(() =>
    backend.pushConnections(
      list([
        ...sample().items,
        row({
          host: "d.example",
          download: 7000,
          process: null,
          seconds: null,
        }),
      ]),
    ),
  );
  await waitFor(() => expect(hosts()).toHaveLength(4));
  expect(hosts()).toEqual(["a.example", "d.example", "b.example", "c.example"]);
  expect(header("Получено")).toHaveAttribute("aria-sort", "descending");
});

it("works together with the search and moves from one column to another", async () => {
  const user = userEvent.setup();
  await shown(new MockBackend());
  await user.type(screen.getByLabelText("Поиск по соединениям"), "tcp");
  expect(hosts()).toEqual(["a.example", "c.example"]);
  await press(user, "Хост");
  await press(user, "Хост");
  expect(hosts()).toEqual(["c.example", "a.example"]);
  await press(user, "Длительность");
  expect(header("Хост")).not.toHaveAttribute("aria-sort");
  expect(header("Длительность")).toHaveAttribute("aria-sort", "ascending");
  expect(hosts()).toEqual(["c.example", "a.example"]);
});
