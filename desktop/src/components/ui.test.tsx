import { useRef, useState } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { ConfirmationDialog, Dialog, Toggle } from "./ui";

// jsdom has no Escape handling for <dialog>: the browser's reaction to Escape is the
// `cancel` event (then `close` when the dialog is closed anyway), which these tests send.
const escape = () =>
  fireEvent(
    screen.getByRole("dialog"),
    new Event("cancel", { cancelable: true }),
  );

it("closes on Escape when idle", () => {
  const onClose = vi.fn();
  render(
    <Dialog title="Диалог" onClose={onClose}>
      Текст
    </Dialog>,
  );
  expect(screen.getByRole("dialog")).toHaveAttribute(
    "closedby",
    "closerequest",
  );
  escape();
  expect(onClose).toHaveBeenCalledOnce();
});

it("ignores Escape and tells the browser so while the dialog is busy", () => {
  const onClose = vi.fn();
  render(
    <ConfirmationDialog
      title="Заменить?"
      message="Текст"
      loading
      onConfirm={vi.fn()}
      onClose={onClose}
    />,
  );
  expect(screen.getByRole("dialog")).toHaveAttribute("closedby", "none");
  escape();
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Закрыть диалог" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Отмена" })).toBeDisabled();
});

it("accepts Escape again once the operation is over", () => {
  const onClose = vi.fn();
  const props = { title: "Заменить?", message: "Текст", onConfirm: vi.fn() };
  const { rerender } = render(
    <ConfirmationDialog {...props} loading onClose={onClose} />,
  );
  escape();
  rerender(<ConfirmationDialog {...props} loading={false} onClose={onClose} />);
  expect(screen.getByRole("dialog")).toHaveAttribute(
    "closedby",
    "closerequest",
  );
  escape();
  expect(onClose).toHaveBeenCalledOnce();
});

it("puts a busy dialog back when the browser closed it anyway", () => {
  render(
    <Dialog title="Диалог" onClose={vi.fn()} closeDisabled>
      Текст
    </Dialog>,
  );
  const dialog = screen.getByRole("dialog");
  // Chromium closes a `cancel`-guarded dialog after repeated Escape presses.
  dialog.removeAttribute("open");
  fireEvent(dialog, new Event("close"));
  expect(dialog).toHaveAttribute("open");
});

it("lets an idle dialog stay closed after the browser closed it", () => {
  render(
    <Dialog title="Диалог" onClose={vi.fn()}>
      Текст
    </Dialog>,
  );
  const dialog = screen.getByRole("dialog", { hidden: true });
  dialog.removeAttribute("open");
  fireEvent(dialog, new Event("close"));
  expect(dialog).not.toHaveAttribute("open");
});

function Page({ withReturnFocus }: { withReturnFocus: boolean }) {
  const opener = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button ref={opener} onClick={() => setOpen(true)}>
        Открыть
      </button>
      <button>Другая</button>
      {open && (
        <Dialog
          title="Диалог"
          onClose={() => setOpen(false)}
          returnFocus={withReturnFocus ? opener : undefined}
        >
          Текст
        </Dialog>
      )}
    </>
  );
}

it("returns focus to the given control when focus was elsewhere as the dialog opened", async () => {
  // The browser drops the focus of a control that is disabled while its operation runs,
  // so the element focused at open time is not the one the user came from.
  render(<Page withReturnFocus />);
  screen.getByRole("button", { name: "Другая" }).focus();
  fireEvent.click(screen.getByRole("button", { name: "Открыть" }));
  await userEvent.click(screen.getByRole("button", { name: "Закрыть диалог" }));
  expect(screen.getByRole("button", { name: "Открыть" })).toHaveFocus();
});

it("falls back to the element that was focused when the dialog opened", async () => {
  render(<Page withReturnFocus={false} />);
  screen.getByRole("button", { name: "Другая" }).focus();
  fireEvent.click(screen.getByRole("button", { name: "Открыть" }));
  await userEvent.click(screen.getByRole("button", { name: "Закрыть диалог" }));
  expect(screen.getByRole("button", { name: "Другая" })).toHaveFocus();
});

it("a switch inside a form turns without sending the form", async () => {
  const user = userEvent.setup();
  const onSubmit = vi.fn((event: { preventDefault: () => void }) =>
    event.preventDefault(),
  );
  function Form() {
    const [on, setOn] = useState(true);
    return (
      <form onSubmit={onSubmit}>
        <Toggle label="Включена" checked={on} onChange={setOn} />
      </form>
    );
  }
  render(<Form />);
  await user.click(screen.getByRole("switch", { name: "Включена" }));
  expect(screen.getByRole("switch", { name: "Включена" })).toHaveAttribute(
    "aria-checked",
    "false",
  );
  expect(onSubmit).not.toHaveBeenCalled();
});
