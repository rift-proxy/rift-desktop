import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { HistoryRow } from "./backend/types";
import { useServerHistory } from "./components/history";
import { MockBackend } from "./testing/mock";

const rows = (...entries: [number, number | null, number | null, string?][]) =>
  entries.map(([at, endpoint_ms, proxy_ms, failure_code]): HistoryRow => ({
    at,
    endpoint_ms,
    proxy_ms,
    failure_code: failure_code ?? null,
  }));
const FIRST = 1789742400;

it("lists the latest measurements of the server in use in the details of the connection, newest first", async () => {
  const backend = new MockBackend();
  backend.rows.set(
    "a",
    rows(
      [FIRST, 30, 90],
      [FIRST + 60, 31, 64],
      [FIRST + 120, 33, null, "proxy_timeout"],
    ),
  );
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  const table = await screen.findByRole("table", {
    name: "Последние замеры сервера",
  });
  const cells = within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) =>
      within(row)
        .getAllByRole("cell")
        .map((cell) => cell.textContent),
    );
  expect(cells).toHaveLength(3);
  // The newest is the failed one; its reason is a sentence, not a code.
  expect(cells[0][1]).toBe("33 мс");
  expect(cells[0][2]).toBe("—");
  expect(cells[0][3]).not.toMatch(/proxy_timeout/);
  expect(cells[0][3]).toMatch(/слишком много времени/);
  expect(cells[1].slice(1)).toEqual(["31 мс", "64 мс", "Успешно"]);
  expect(cells[2].slice(1)).toEqual(["30 мс", "90 мс", "Успешно"]);
  expect(backend.historyAsked).toEqual(["a"]);
});

it("says so when a server has no measurements yet", async () => {
  const backend = new MockBackend();
  backend.rows.set("a", []);
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  expect(await screen.findByText("Замеров пока нет.")).toBeInTheDocument();
  expect(screen.queryByRole("table", { name: /замеры/i })).toBeNull();
});

it("asks again when the server is checked again, and for another server when it is chosen", async () => {
  const backend = new MockBackend();
  backend.rows.set("a", rows([FIRST, 30, 90]));
  backend.rows.set("b", rows([FIRST, 39, null, "proxy_timeout"]));
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  await screen.findByRole("table", { name: "Последние замеры сервера" });
  expect(backend.historyAsked).toEqual(["a"]);

  backend.rows.set("a", rows([FIRST, 30, 90], [FIRST + 60, 31, 64]));
  backend.update((state) => {
    state.servers.find((s) => s.id === "a")!.checked_at = FIRST + 60;
  });
  await waitFor(() => expect(backend.historyAsked).toEqual(["a", "a"]));
  await waitFor(() =>
    expect(
      within(
        screen.getByRole("table", { name: "Последние замеры сервера" }),
      ).getAllByRole("row"),
    ).toHaveLength(3),
  );

  // A snapshot in which nothing was checked does not ask again.
  backend.update((state) => {
    state.next_check = 5;
  });
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(backend.historyAsked).toEqual(["a", "a"]);

  backend.update((state) => {
    state.selected = "b";
  });
  await waitFor(() => expect(backend.historyAsked.at(-1)).toBe("b"));
});

it("shows the measurements of any server on the tab of the checks, the server in use first", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.rows.set("a", rows([FIRST, 30, 90]));
  backend.rows.set("b", rows([FIRST, 39, null, "proxy_timeout"]));
  render(
    <BackendProvider client={backend}>
      <App initialPage="journal" />
    </BackendProvider>,
  );
  expect(screen.queryByText("Замеры серверов")).toBeNull();
  await user.click(
    await screen.findByRole("button", { name: "Проверки серверов" }),
  );
  await user.click(await screen.findByText("Замеры серверов"));
  const server = screen.getByLabelText("Сервер");
  expect(server).toHaveValue("a");
  expect(
    await screen.findByRole("table", { name: "Последние замеры сервера" }),
  ).toBeVisible();
  await user.selectOptions(server, "b");
  await waitFor(() => expect(backend.historyAsked.at(-1)).toBe("b"));
  const table = await screen.findByRole("table", {
    name: "Последние замеры сервера",
  });
  expect(await within(table).findByText("39 мс")).toBeVisible();
  expect(within(table).getByText(/слишком много времени/)).toBeVisible();
  // Other tabs do not carry it.
  await user.click(screen.getByRole("button", { name: "Ядро" }));
  expect(screen.queryByText("Замеры серверов")).toBeNull();
});

it("keeps the screen alive when the history cannot be read", async () => {
  const backend = new MockBackend();
  backend.history = () => Promise.reject(new Error("боль"));
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  expect(await screen.findByText("Замеров пока нет.")).toBeInTheDocument();
  await waitFor(() =>
    expect(backend.loggedErrors).toContain(
      "Не удалось получить историю замеров",
    ),
  );
  expect(backend.loggedDetails[0]).toContain("боль");
  await userEvent.setup().click(screen.getByText("Подробности соединения"));
  expect(screen.getByText("Замеров пока нет.")).toBeVisible();
  expect(screen.getByRole("navigation")).toBeVisible();
  expect(screen.getByRole("button", { name: "Отключить" })).toBeVisible();
});

function HistoryProbe({ id }: { id: string }) {
  const rows = useServerHistory(id, null);
  return <output>{rows.length}</output>;
}

it.each(["changed", "unmounted"])(
  "ignores a stale history failure after the view is %s",
  async (change) => {
    const backend = new MockBackend();
    let reject!: (error: Error) => void;
    backend.history = (id) =>
      id === "a"
        ? new Promise((_, fail) => {
            reject = fail;
          })
        : Promise.resolve([]);
    const tree = (id: string) => (
      <BackendProvider client={backend}>
        <HistoryProbe id={id} />
      </BackendProvider>
    );
    const view = render(tree("a"));
    if (change === "changed") view.rerender(tree("b"));
    else view.unmount();
    await act(async () => {
      reject(new Error("late"));
    });
    expect(backend.loggedErrors).toEqual([]);
  },
);
