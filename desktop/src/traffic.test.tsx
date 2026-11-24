import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import { MockBackend, fixture } from "./testing/mock";

const mount = (backend: MockBackend) =>
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
/** The speed fact of the connection screen. */
const fact = async () => {
  const label = await screen.findByText("Скорость", {
    selector: ".fact-label",
  });
  return within(label.parentElement as HTMLElement);
};

it("shows dashes until a speed arrives, then the speed in both directions", async () => {
  const backend = new MockBackend();
  mount(backend);
  const speeds = await fact();
  expect(speeds.getAllByText("—")).toHaveLength(2);
  await waitFor(() => expect(backend.watching).toEqual([true]));

  act(() => backend.pushTraffic({ up: 1536, down: 3 * 1024 ** 2 }));
  expect(speeds.getByText("3 МБ/с")).toBeVisible();
  expect(speeds.getByText("1,5 КБ/с")).toBeVisible();
  // The direction is told in words as well, for a screen reader.
  expect(speeds.getByText("Скачивание").nextSibling).toHaveTextContent(
    "3 МБ/с",
  );
  expect(speeds.getByText("Отдача").nextSibling).toHaveTextContent("1,5 КБ/с");

  act(() => backend.pushTraffic(null));
  expect(speeds.getAllByText("—")).toHaveLength(2);
});

it("asks for nothing while disconnected", async () => {
  const backend = new MockBackend({
    ...fixture,
    status: "disconnected",
    runtime_active: false,
  });
  mount(backend);
  const speeds = await fact();
  expect(speeds.getAllByText("—")).toHaveLength(2);
  await new Promise((resolve) => setTimeout(resolve, 30));
  expect(backend.watching).toEqual([]);
});

it("stops watching when the screen is left and starts again when it returns", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  mount(backend);
  await waitFor(() => expect(backend.watching).toEqual([true]));
  act(() => backend.pushTraffic({ up: 1024, down: 2048 }));
  expect((await fact()).getByText("1 КБ/с")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Серверы" }));
  await waitFor(() => expect(backend.watching).toEqual([true, false]));
  act(() => backend.pushTraffic({ up: 1, down: 2 }));
  await user.click(screen.getByRole("button", { name: "Подключение" }));
  await waitFor(() => expect(backend.watching).toEqual([true, false, true]));
  // What was shown before the screen was left is not shown again.
  expect((await fact()).getAllByText("—")).toHaveLength(2);
});

it("stops watching when Rift disconnects", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  mount(backend);
  await waitFor(() => expect(backend.watching).toEqual([true]));
  act(() => backend.pushTraffic({ up: 10, down: 20 }));
  await user.click(await screen.findByRole("button", { name: "Отключить" }));
  await waitFor(() => expect(backend.watching).toEqual([true, false]));
  expect((await fact()).getAllByText("—")).toHaveLength(2);
});
