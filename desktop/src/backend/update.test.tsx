import { expect, it, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { BackendProvider } from "./provider";
import { useUpdateInfo } from "./update";
import type { BackendClient, UpdateInfo } from "./types";

function View() {
  const { info, check, dismiss, install } = useUpdateInfo();
  const [installError, setInstallError] = useState<string | null>(null);
  return (
    <>
      <output>{info?.version ?? "none"}</output>
      <button onClick={() => void check()}>Check</button>
      <button onClick={() => void dismiss()}>Dismiss</button>
      <button
        onClick={() =>
          void install().catch((e: unknown) =>
            setInstallError(typeof e === "string" ? e : "error"),
          )
        }
      >
        Install
      </button>
      {installError && (
        <output data-testid="install-error">{installError}</output>
      )}
    </>
  );
}

function client(overrides: Partial<BackendClient> = {}): BackendClient {
  return {
    snapshot: () => Promise.reject("unused"),
    execute: () => Promise.reject("unused"),
    preview: () => Promise.reject("unused"),
    subscribe: () => Promise.resolve(() => {}),
    logError: () => Promise.resolve(),
    exportProfile: () => Promise.resolve(false),
    inspectProfile: () => Promise.resolve(null),
    importProfile: () => Promise.reject("unused"),
    ...overrides,
  };
}

it("shows a pushed update", async () => {
  let push!: (info: UpdateInfo) => void;
  const api = client({
    onUpdate: (listener) => {
      push = listener;
      return Promise.resolve(() => {});
    },
  });
  render(
    <BackendProvider client={api}>
      <View />
    </BackendProvider>,
  );
  await screen.findByText("none");
  act(() => push({ version: "0.2.0", notes: null, pub_date: null }));
  expect(await screen.findByText("0.2.0")).toBeInTheDocument();
});

it("keeps a newer pushed update when a stale manual check resolves late", async () => {
  let push!: (info: UpdateInfo) => void;
  let resolveCheck!: (info: UpdateInfo | null) => void;
  const api = client({
    onUpdate: (listener) => {
      push = listener;
      return Promise.resolve(() => {});
    },
    checkForUpdate: () => new Promise((resolve) => (resolveCheck = resolve)),
  });
  const user = userEvent.setup();
  render(
    <BackendProvider client={api}>
      <View />
    </BackendProvider>,
  );
  await user.click(screen.getByRole("button", { name: "Check" }));
  act(() => push({ version: "0.3.0", notes: null, pub_date: null }));
  await act(async () =>
    resolveCheck({ version: "0.2.0", notes: null, pub_date: null }),
  );
  expect(screen.getByText("0.3.0")).toBeInTheDocument();
});

it("dismisses through the client and clears the banner", async () => {
  let push!: (info: UpdateInfo) => void;
  const dismissUpdate = vi.fn(() => Promise.resolve());
  const api = client({
    onUpdate: (listener) => {
      push = listener;
      return Promise.resolve(() => {});
    },
    dismissUpdate,
  });
  const user = userEvent.setup();
  render(
    <BackendProvider client={api}>
      <View />
    </BackendProvider>,
  );
  act(() => push({ version: "0.2.0", notes: null, pub_date: null }));
  await screen.findByText("0.2.0");
  await user.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(dismissUpdate).toHaveBeenCalledWith("0.2.0");
  expect(screen.getByText("none")).toBeInTheDocument();
});

it("rejects install when the host has no updater", async () => {
  render(
    <BackendProvider client={client()}>
      <View />
    </BackendProvider>,
  );
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Install" }));
  expect(await screen.findByTestId("install-error")).toHaveTextContent(
    "Обновления недоступны",
  );
});
