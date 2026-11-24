import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import { UpdateBanner } from "./update";
import { nextVersion } from "../testing/mock";

const meta = {
  title: "Rift/Обновление",
  component: UpdateBanner,
  parameters: { layout: "padded" },
} satisfies Meta<typeof UpdateBanner>;
export default meta;
type Story = StoryObj<typeof meta>;

const info = {
  version: nextVersion,
  notes: "Исправления и улучшения",
  pub_date: null,
};

export const Banner: Story = {
  args: {
    info,
    onDismiss: () => {},
    onInstall: () => Promise.resolve(),
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await expect(c.getByText(`Доступна версия ${nextVersion}.`)).toBeVisible();
  },
};

export const ConfirmInstall: Story = {
  args: {
    info,
    onDismiss: () => {},
    onInstall: () => Promise.resolve(),
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(c.getByRole("button", { name: "Установить" }));
    await expect(
      within(document.body).getByRole("heading", {
        name: `Установить версию ${nextVersion}?`,
      }),
    ).toBeVisible();
  },
};

export const Installing: Story = {
  args: {
    info,
    onDismiss: () => {},
    // Never resolves: holds the dialog in its "installing" state for the keyboard/visual check.
    onInstall: () => new Promise(() => {}),
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(c.getByRole("button", { name: "Установить" }));
    const body = within(document.body);
    await userEvent.click(
      body.getByRole("button", { name: "Установить и перезапустить" }),
    );
    await expect(body.getByRole("button", { name: "Отмена" })).toBeDisabled();
    await expect(
      body.getByRole("button", { name: "Закрыть диалог" }),
    ).toBeDisabled();
  },
};

export const InstallFailed: Story = {
  args: {
    info,
    onDismiss: () => {},
    onInstall: () => Promise.reject("Сеть недоступна"),
  },
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(c.getByRole("button", { name: "Установить" }));
    const body = within(document.body);
    await userEvent.click(
      body.getByRole("button", { name: "Установить и перезапустить" }),
    );
    await expect(await body.findByRole("alert")).toHaveTextContent(
      "Сеть недоступна",
    );
  },
};

export const DarkBanner: Story = {
  ...Banner,
  parameters: { theme: "dark" },
};
export const DarkConfirmInstall: Story = {
  ...ConfirmInstall,
  parameters: { theme: "dark" },
};
export const DarkInstalling: Story = {
  ...Installing,
  parameters: { theme: "dark" },
};
export const DarkInstallFailed: Story = {
  ...InstallFailed,
  parameters: { theme: "dark" },
};
