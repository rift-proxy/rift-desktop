import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import { Plus } from "lucide-react";
import * as UI from "./ui";
const meta = {
  title: "Rift/Компоненты",
  parameters: { layout: "padded" },
} satisfies Meta;
export default meta;
type Story = StoryObj;
export const Buttons: Story = {
  render: () => (
    <UI.Toolbar>
      <UI.Button variant="primary">Подключить</UI.Button>
      <UI.Button>Проверить</UI.Button>
      <UI.Button variant="ghost">Открыть</UI.Button>
      <UI.Button variant="danger">Удалить</UI.Button>
      <UI.Button disabled>Отключена</UI.Button>
      <UI.Button loading>Загрузка</UI.Button>
      <UI.IconButton label="Добавить">
        <Plus size={16} />
      </UI.IconButton>
    </UI.Toolbar>
  ),
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.hover(c.getByRole("button", { name: "Проверить" }));
    await userEvent.tab();
    await expect(c.getByRole("button", { name: "Подключить" })).toHaveFocus();
  },
};
function Controls() {
  const [toggle, setToggle] = useState(false);
  const [mode, setMode] = useState("proxy");
  return (
    <div style={{ maxWidth: 500 }}>
      <UI.Toggle label="Автовыбор" checked={toggle} onChange={setToggle} />
      <UI.Checkbox label="Запомнить" />
      <UI.Checkbox label="Недоступно" disabled />
      <UI.Radio label="Вариант 1" name="option" defaultChecked />
      <UI.Radio label="Вариант 2" name="option" />
      <UI.Segmented
        label="Режим"
        value={mode}
        onChange={setMode}
        options={[
          { label: "Proxy", value: "proxy" },
          { label: "TUN", value: "tun" },
          { label: "TUN + Proxy", value: "tun_proxy" },
        ]}
      />
      <UI.Field label="Название" placeholder="Название сервера" />
      <UI.Field label="URL" type="url" placeholder="https://example.com" />
      <UI.Field label="Поиск" type="search" />
      <UI.Field label="Ошибка" error="Проверьте адрес" />
      <UI.Field label="Отключено" disabled />
      <UI.Select label="Тема">
        <option>Светлая</option>
        <option>Тёмная</option>
      </UI.Select>
    </div>
  );
}
export const Inputs: Story = {
  render: () => <Controls />,
  play: async ({ canvasElement }) => {
    const c = within(canvasElement);
    await userEvent.click(c.getByRole("switch"));
    await expect(c.getByRole("switch")).toHaveAttribute("aria-checked", "true");
  },
};
export const Statuses: Story = {
  render: () => (
    <>
      <UI.Toolbar>
        {["online", "failed", "unreachable", "reachable", "unchecked"].map(
          (s) => (
            <UI.StatusChip key={s} status={s} />
          ),
        )}
      </UI.Toolbar>
      <UI.Alert>Не удалось обновить подписку</UI.Alert>
      <UI.Alert tone="info">Проверка серверов…</UI.Alert>
      <UI.Notification onClose={() => {}}>Настройки сохранены</UI.Notification>
      <UI.LatencyDisplay endpoint={31} proxy={64} />
      <UI.ProxyAddress port={2080} />
    </>
  ),
};
export const Empty: Story = {
  render: () => (
    <UI.EmptyState
      title="Нет серверов"
      description="Добавьте подписку или сервер вручную"
    />
  ),
};
export const Loading: Story = { render: () => <UI.LoadingState /> };
export const ErrorState: Story = {
  render: () => <UI.ErrorState message="Проверьте подключение" />,
};
export const Confirm: Story = {
  render: () => (
    <UI.ConfirmationDialog
      title="Удалить сервер?"
      message="Сервер будет удалён из ручного списка."
      onClose={() => {}}
      onConfirm={() => {}}
    />
  ),
};

export const DarkButtons: Story = { ...Buttons, parameters: { theme: "dark" } };
export const DarkInputs: Story = { ...Inputs, parameters: { theme: "dark" } };
export const DarkStatuses: Story = {
  ...Statuses,
  parameters: { theme: "dark" },
};
export const DarkEmpty: Story = { ...Empty, parameters: { theme: "dark" } };
export const DarkLoading: Story = { ...Loading, parameters: { theme: "dark" } };
export const DarkErrorState: Story = {
  ...ErrorState,
  parameters: { theme: "dark" },
};
export const DarkConfirm: Story = { ...Confirm, parameters: { theme: "dark" } };
