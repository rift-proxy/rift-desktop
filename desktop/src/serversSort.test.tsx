import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { Snapshot } from "./backend/types";
import { MockBackend, fixture } from "./testing/mock";

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
/** The names in the first column, top to bottom. */
const names = () =>
  screen
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell")[0].textContent);
const heading = (name: string) => screen.getByRole("columnheader", { name });
const sortedBy = (name: string) => heading(name).getAttribute("aria-sort");

// proxy_ms in the fixture: Frankfurt 64, Helsinki none, Amsterdam 81, Warsaw 74.
const byName = ["Amsterdam 01", "Frankfurt 01", "Helsinki 02", "Warsaw 03"];

it("lists the servers by name, upwards, until the user chooses another order", async () => {
  mount();
  await screen.findByRole("heading", { name: "Серверы" });
  expect(names()).toEqual(byName);
  expect(sortedBy("Название")).toBe("ascending");
  expect(sortedBy("Задержка")).toBeNull();
  expect(sortedBy("Статус")).toBeNull();
});

it("orders by a column on a click, backwards on the second, and back to the name on the third", async () => {
  const { execute } = mount();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });

  await user.click(screen.getByRole("button", { name: "Задержка" }));
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_servers_sort",
    column: "proxy_ms",
    descending: false,
  });
  await waitFor(() =>
    expect(names()).toEqual([
      "Frankfurt 01",
      "Warsaw 03",
      "Amsterdam 01",
      "Helsinki 02",
    ]),
  );
  expect(sortedBy("Задержка")).toBe("ascending");
  expect(sortedBy("Название")).toBeNull();

  await user.click(screen.getByRole("button", { name: "Задержка" }));
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_servers_sort",
    column: "proxy_ms",
    descending: true,
  });
  // The server that was not measured stays last, backwards too.
  await waitFor(() =>
    expect(names()).toEqual([
      "Amsterdam 01",
      "Warsaw 03",
      "Frankfurt 01",
      "Helsinki 02",
    ]),
  );
  expect(sortedBy("Задержка")).toBe("descending");

  await user.click(screen.getByRole("button", { name: "Задержка" }));
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_servers_sort",
    column: "name",
    descending: false,
  });
  await waitFor(() => expect(names()).toEqual(byName));
  expect(sortedBy("Название")).toBe("ascending");
});

it("makes every column of the detailed list a button that orders by it", async () => {
  const { execute } = mount();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  await user.click(screen.getByRole("button", { name: "Подробности" }));
  const columns = [
    ["Название", "name"],
    ["Протокол", "protocol"],
    ["До сервера", "endpoint_ms"],
    ["Через сервер", "proxy_ms"],
    ["Статус", "status"],
    ["Источник", "source"],
    ["Проверка", "checked_at"],
  ];
  expect(screen.getAllByRole("columnheader")).toHaveLength(8);
  for (const [label, column] of columns) {
    execute.mockClear();
    await user.click(screen.getByRole("button", { name: label }));
    // The name column is already the order, so its first click goes backwards.
    expect(execute, label).toHaveBeenCalledWith({
      type: "set_servers_sort",
      column,
      descending: column === "name" && label === "Название",
    });
  }
  // The column of actions is not one of them.
  expect(
    within(screen.getByRole("columnheader", { name: "Действия" })).queryByRole(
      "button",
    ),
  ).toBeNull();
});

it("orders by protocol and by the source in the detailed list", async () => {
  mount();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  await user.click(screen.getByRole("button", { name: "Подробности" }));
  await user.click(screen.getByRole("button", { name: "Протокол" }));
  // shadowsocks, trojan, vless, vless reality
  await waitFor(() =>
    expect(names()).toEqual([
      "Warsaw 03",
      "Amsterdam 01",
      "Helsinki 02",
      "Frankfurt 01",
    ]),
  );
  await user.click(screen.getByRole("button", { name: "Источник" }));
  // Вручную, Основная, Основная, Резервная: servers of one source go by name.
  await waitFor(() =>
    expect(names()).toEqual([
      "Warsaw 03",
      "Frankfurt 01",
      "Helsinki 02",
      "Amsterdam 01",
    ]),
  );
});

it("opens with the order that was saved", async () => {
  mount({
    ...fixture,
    servers_sort: { column: "proxy_ms", descending: true },
  });
  await screen.findByRole("heading", { name: "Серверы" });
  expect(names()).toEqual([
    "Amsterdam 01",
    "Warsaw 03",
    "Frankfurt 01",
    "Helsinki 02",
  ]);
  expect(sortedBy("Задержка")).toBe("descending");
});

it("orders from the keyboard", async () => {
  const { execute } = mount();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  await user.tab();
  screen.getByRole("button", { name: "Статус" }).focus();
  await user.keyboard("{Enter}");
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_servers_sort",
    column: "status",
    descending: false,
  });
  await user.keyboard(" ");
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_servers_sort",
    column: "status",
    descending: true,
  });
});

it("selects the server that was clicked, in any order", async () => {
  const { execute } = mount({
    ...fixture,
    servers_sort: { column: "proxy_ms", descending: true },
  });
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  execute.mockClear();
  await user.click(screen.getByRole("button", { name: "Warsaw 03" }));
  expect(execute).toHaveBeenCalledTimes(1);
  expect(execute).toHaveBeenCalledWith({ type: "select", id: "d" });
});

it("orders inside the source that is chosen", async () => {
  mount();
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  await user.click(screen.getByRole("button", { name: "Основная 2" }));
  await user.click(screen.getByRole("button", { name: "Задержка" }));
  await user.click(screen.getByRole("button", { name: "Задержка" }));
  await waitFor(() => expect(names()).toEqual(["Frankfurt 01", "Helsinki 02"]));
  expect(sortedBy("Задержка")).toBe("descending");
});

it("says an order by a column the compact list does not show, in words", async () => {
  mount({
    ...fixture,
    servers_sort: { column: "source", descending: false },
  });
  const user = userEvent.setup();
  await screen.findByRole("heading", { name: "Серверы" });
  // No heading of the compact list carries it, so the table says so itself.
  expect(
    screen.getByText("Порядок: Источник, по возрастанию"),
  ).toBeInTheDocument();
  expect(screen.queryByRole("columnheader", { name: "Источник" })).toBeNull();
  // The detailed list has the column, and its own indicator.
  await user.click(screen.getByRole("button", { name: "Подробности" }));
  expect(screen.queryByText(/^Порядок: /)).toBeNull();
  expect(sortedBy("Источник")).toBe("ascending");
});

it("says the direction of an order by a hidden column, and says nothing of the ones in view", async () => {
  mount({
    ...fixture,
    servers_sort: { column: "checked_at", descending: true },
  });
  await screen.findByRole("heading", { name: "Серверы" });
  expect(
    screen.getByText("Порядок: Проверка, по убыванию"),
  ).toBeInTheDocument();
  cleanup();
  mount({ ...fixture, servers_sort: { column: "proxy_ms", descending: true } });
  await screen.findByRole("heading", { name: "Серверы" });
  expect(screen.queryByText(/^Порядок: /)).toBeNull();
});

it("puts the server in use first when ordered by status", async () => {
  // Helsinki failed its last check and is the one in use: it still leads.
  const helsinki = fixture.servers.find((s) => s.name === "Helsinki 02")!;
  mount({
    ...fixture,
    selected: helsinki.id,
    servers_sort: { column: "status", descending: false },
  });
  await screen.findByRole("heading", { name: "Серверы" });
  expect(names()[0]).toBe("Helsinki 02");
});
