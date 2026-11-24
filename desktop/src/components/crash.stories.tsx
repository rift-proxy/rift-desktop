import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, within } from "storybook/test";
import { ErrorBoundary } from "./errors";

function Broken(): never {
  throw new Error("Storybook: падение рендера");
}

// What the main window shows when rendering fails (ErrorBoundary wraps the whole App).
const meta = {
  title: "Rift/Сбой интерфейса",
  parameters: { layout: "fullscreen" },
} satisfies Meta;
export default meta;
type Story = StoryObj;

export const MainWindowCrash: Story = {
  render: () => (
    <ErrorBoundary logError={() => {}}>
      <Broken />
    </ErrorBoundary>
  ),
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    const alert = await c.findByRole("alert");
    await expect(alert).toHaveTextContent("Интерфейс Rift остановился");
    const rect = alert.getBoundingClientRect();
    await expect(
      Math.abs(rect.top + rect.height / 2 - window.innerHeight / 2),
    ).toBeLessThan(2);
  },
};

export const DarkMainWindowCrash: Story = {
  ...MainWindowCrash,
  parameters: { theme: "dark" },
};
