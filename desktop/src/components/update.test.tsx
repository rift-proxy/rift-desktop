import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { UpdateBanner } from "./update";

const info = { version: "0.2.0", notes: "Исправления", pub_date: null };

it("shows the version and dismisses through the callback", async () => {
  const onDismiss = vi.fn();
  render(
    <UpdateBanner info={info} onDismiss={onDismiss} onInstall={vi.fn()} />,
  );
  expect(screen.getByText("Доступна версия 0.2.0.")).toBeInTheDocument();
  await userEvent.click(
    screen.getByRole("button", { name: "Закрыть уведомление" }),
  );
  expect(onDismiss).toHaveBeenCalled();
});

it("asks for confirmation before installing, and cancel does not install", async () => {
  const onInstall = vi.fn(() => Promise.resolve());
  render(
    <UpdateBanner info={info} onDismiss={vi.fn()} onInstall={onInstall} />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Установить" }));
  expect(
    screen.getByRole("heading", { name: "Установить версию 0.2.0?" }),
  ).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Отмена" }));
  expect(onInstall).not.toHaveBeenCalled();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("installs on confirm", async () => {
  const onInstall = vi.fn(() => Promise.resolve());
  render(
    <UpdateBanner info={info} onDismiss={vi.fn()} onInstall={onInstall} />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Установить" }));
  await userEvent.click(
    screen.getByRole("button", { name: "Установить и перезапустить" }),
  );
  expect(onInstall).toHaveBeenCalled();
});

it("keeps the dialog open and shows the failure when install rejects", async () => {
  const onInstall = vi.fn(() => Promise.reject("Обновление не найдено"));
  render(
    <UpdateBanner info={info} onDismiss={vi.fn()} onInstall={onInstall} />,
  );
  await userEvent.click(screen.getByRole("button", { name: "Установить" }));
  await userEvent.click(
    screen.getByRole("button", { name: "Установить и перезапустить" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Обновление не найдено",
  );
  expect(screen.getByRole("dialog")).toBeInTheDocument();
});
