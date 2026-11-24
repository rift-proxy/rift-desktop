import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { App, type Page } from "./App";
import type { Snapshot } from "./backend/types";
import { BackendProvider } from "./backend/provider";
import { MockBackend, fixture } from "./testing/mock";
import { installWindowGuards } from "./windowGuards";

function mount(state: Snapshot = fixture, page: Page = "servers") {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage={page} />
    </BackendProvider>,
  );
  return { backend, execute };
}
const rowOf = async (name: string) =>
  (await screen.findByRole("button", { name })).closest("tr")!;
const names = () =>
  screen
    .getAllByRole("button", { name: /\d\d$/ })
    .map((b) => b.textContent?.trim());

let removeGuards: (() => void) | null = null;
afterEach(() => {
  removeGuards?.();
  removeGuards = null;
});

it("opens a menu with one item on a right click and keeps the server out of Auto Select", async () => {
  // The guard that closes the browser's own menu is on, as in the application.
  removeGuards = installWindowGuards();
  const user = userEvent.setup();
  const { execute } = mount();
  const row = await rowOf("Frankfurt 01");
  fireEvent.contextMenu(row, { clientX: 120, clientY: 90 });
  const menu = screen.getByRole("menu", { name: "Frankfurt 01" });
  const items = within(menu).getAllByRole("menuitem");
  expect(items.map((i) => i.textContent)).toEqual(["Исключить из автовыбора"]);
  expect(items[0]).toHaveFocus();
  await user.click(items[0]);
  expect(execute).toHaveBeenCalledExactlyOnceWith({
    type: "set_server_excluded",
    id: "a",
    excluded: true,
  });
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
});

it("shows a kept-out server grey, marked, last, and offers to bring it back", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  fireEvent.contextMenu(await rowOf("Frankfurt 01"), {
    clientX: 10,
    clientY: 10,
  });
  await user.click(screen.getByRole("menuitem"));
  const row = await rowOf("Frankfurt 01");
  expect(row).toHaveClass("excluded");
  expect(within(row).getByText("Исключён")).toBeVisible();
  // The order by name would put it second; it is last.
  expect(names().at(-1)).toBe("Frankfurt 01");
  // A server kept out can still be chosen by hand.
  await user.click(within(row).getByRole("button", { name: "Frankfurt 01" }));
  expect(execute).toHaveBeenCalledWith({ type: "select", id: "a" });
  // And the menu of it says the opposite.
  fireEvent.contextMenu(row, { clientX: 10, clientY: 10 });
  await user.click(
    screen.getByRole("menuitem", { name: "Вернуть в автовыбор" }),
  );
  expect(execute).toHaveBeenLastCalledWith({
    type: "set_server_excluded",
    id: "a",
    excluded: false,
  });
  expect(await rowOf("Frankfurt 01")).not.toHaveClass("excluded");
  expect(screen.queryByText("Исключён")).not.toBeInTheDocument();
});

it.each([
  ["the menu key", "{ContextMenu}"],
  ["Shift+F10", "{Shift>}{F10}{/Shift}"],
])(
  "opens the menu from the keyboard with %s and gives the focus back on Escape",
  async (_, keys) => {
    const user = userEvent.setup();
    mount();
    const name = await screen.findByRole("button", { name: "Warsaw 03" });
    name.focus();
    await user.keyboard(keys);
    const item = screen.getByRole("menuitem", {
      name: "Исключить из автовыбора",
    });
    expect(item).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(name).toHaveFocus();
  },
);

it("opens the menu once when the key also makes the browser send a context menu event", async () => {
  const user = userEvent.setup();
  mount();
  const name = await screen.findByRole("button", { name: "Warsaw 03" });
  name.focus();
  await user.keyboard("{ContextMenu}");
  // The platform follows the key with the same event a right click makes, at no position.
  fireEvent.contextMenu(name, { clientX: 0, clientY: 0 });
  expect(screen.getAllByRole("menu")).toHaveLength(1);
});

it("closes on a click elsewhere, on Tab and when another row asks for its own menu", async () => {
  const user = userEvent.setup();
  mount();
  fireEvent.contextMenu(await rowOf("Warsaw 03"), { clientX: 10, clientY: 10 });
  expect(screen.getByRole("menu")).toBeVisible();
  await user.click(document.body);
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  fireEvent.contextMenu(await rowOf("Warsaw 03"), { clientX: 10, clientY: 10 });
  await user.keyboard("{Tab}");
  expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  fireEvent.contextMenu(await rowOf("Warsaw 03"), { clientX: 10, clientY: 10 });
  fireEvent.contextMenu(await rowOf("Helsinki 02"), {
    clientX: 30,
    clientY: 30,
  });
  const menus = screen.getAllByRole("menu");
  expect(menus).toHaveLength(1);
  expect(menus[0]).toHaveAccessibleName("Helsinki 02");
});

it("takes the menu to the screen when the click is at its edge", async () => {
  mount();
  fireEvent.contextMenu(await rowOf("Warsaw 03"), {
    clientX: window.innerWidth + 400,
    clientY: window.innerHeight + 400,
  });
  const menu = screen.getByRole("menu");
  expect(parseFloat(menu.style.left)).toBeLessThanOrEqual(window.innerWidth);
  expect(parseFloat(menu.style.top)).toBeLessThanOrEqual(window.innerHeight);
});
