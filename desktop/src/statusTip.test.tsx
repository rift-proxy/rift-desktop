import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { Snapshot } from "./backend/types";
import { dateTime, statusTitle } from "./format";
import { MockBackend, fixture } from "./testing/mock";

function mount(state: Snapshot = fixture) {
  const backend = new MockBackend(state);
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  return backend;
}
const chipOf = async (server: string) => {
  const row = (await screen.findByRole("button", { name: server })).closest(
    "tr",
  )!;
  return within(row).getByTestId("status-chip");
};

it("names the moment a server was checked, with the date, and says so when it was not", () => {
  expect(statusTitle(1789742400)).toBe(`Проверено ${dateTime(1789742400)}`);
  expect(statusTitle(1789742400)).toMatch(
    /\d{2}\.\d{2}\.\d{4} \d{2}:\d{2}:\d{2}/,
  );
  expect(statusTitle(null)).toBe("Ещё не проверялся");
});

it("shows when the status was set on hover, for every kind of status", async () => {
  const user = userEvent.setup();
  const statuses = [
    "online",
    "failed",
    "unreachable",
    "reachable",
    "connected",
  ] as const;
  const servers = statuses.map((status, i) => ({
    ...fixture.servers[0],
    id: `s${i}`,
    name: `Server ${i}`,
    status,
    checked_at: 1789742400 + i * 60,
  }));
  mount({ ...fixture, servers, selected: null });
  for (const [i] of statuses.entries()) {
    const chip = await chipOf(`Server ${i}`);
    await user.hover(chip);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      `Проверено ${dateTime(1789742400 + i * 60)}`,
    );
    await user.unhover(chip);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  }
});

it("says a server that was never checked was never checked", async () => {
  const user = userEvent.setup();
  mount({
    ...fixture,
    servers: [{ ...fixture.servers[0], status: "unchecked", checked_at: null }],
  });
  await user.hover(await chipOf("Frankfurt 01"));
  expect(await screen.findByRole("tooltip")).toHaveTextContent(
    "Ещё не проверялся",
  );
});

it("shows it on keyboard focus too and closes it on Escape", async () => {
  const user = userEvent.setup();
  mount();
  const chip = await chipOf("Frankfurt 01");
  expect(chip).toHaveAttribute("tabindex", "0");
  act(() => chip.focus());
  const tip = await screen.findByRole("tooltip");
  expect(tip).toHaveTextContent(`Проверено ${dateTime(1789742400)}`);
  expect(chip).toHaveAccessibleDescription(tip.textContent ?? "");
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  act(() => chip.blur());
});

it("changes with the status after a new check", async () => {
  const user = userEvent.setup();
  const backend = mount();
  const chip = await chipOf("Frankfurt 01");
  await user.hover(chip);
  expect(await screen.findByRole("tooltip")).toHaveTextContent(
    dateTime(1789742400),
  );
  await user.unhover(chip);
  await act(async () => {
    backend.update((state) => {
      state.servers = state.servers.map((s) =>
        s.id === "a" ? { ...s, checked_at: 1789746000, status: "failed" } : s,
      );
    });
  });
  const again = await chipOf("Frankfurt 01");
  fireEvent.mouseEnter(again);
  expect(await screen.findByRole("tooltip")).toHaveTextContent(
    dateTime(1789746000),
  );
});

// jsdom has no layout: the mark and the tip get the boxes the test gives them, and the window is
// the size jsdom says (1024 by 768).
function withBoxes(mark: { left: number; top: number; width: number }) {
  const rect = (left: number, top: number, width: number, height: number) =>
    ({
      left,
      top,
      width,
      height,
      right: left + width,
      bottom: top + height,
      x: left,
      y: top,
      toJSON: () => ({}),
    }) as DOMRect;
  return vi
    .spyOn(HTMLElement.prototype, "getBoundingClientRect")
    .mockImplementation(function (this: HTMLElement) {
      return this.getAttribute("role") === "tooltip"
        ? rect(0, 0, 200, 28)
        : rect(mark.left, mark.top, mark.width, 24);
    });
}
const tipOf = async () => {
  const tip = await screen.findByRole("tooltip");
  return { left: parseFloat(tip.style.left), top: parseFloat(tip.style.top) };
};

it("puts the tip under the mark, in a class of its own that does not share the in-flow tooltip's rules", async () => {
  const user = userEvent.setup();
  const boxes = withBoxes({ left: 300, top: 100, width: 80 });
  mount();
  await user.hover(await chipOf("Frankfurt 01"));
  const tip = await screen.findByRole("tooltip");
  expect(tip).toHaveClass("tip-bubble");
  expect(tip).not.toHaveClass("tooltip");
  expect(await tipOf()).toEqual({ left: 300, top: 100 + 24 + 6 });
  boxes.mockRestore();
});

it("keeps the tip inside the window: above a mark near the bottom, left of the right edge", async () => {
  const user = userEvent.setup();
  const boxes = withBoxes({ left: 1000, top: 760, width: 80 });
  mount();
  await user.hover(await chipOf("Frankfurt 01"));
  // The window is 1024 by 768: 200 px of tip do not fit to the right of 1000, 28 px do not fit
  // below 784.
  expect(await tipOf()).toEqual({ left: 1024 - 200, top: 760 - 6 - 28 });
  boxes.mockRestore();
});
