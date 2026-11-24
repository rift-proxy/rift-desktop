import { expect, it } from "vitest";
import { render, screen, act, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { BackendProvider, useAppState } from "./provider";
import type { BackendClient, Snapshot } from "./types";
import { fixture } from "../testing/mock";
function View() {
  const { state, execute } = useAppState();
  return (
    <>
      <output>{state?.status}</output>
      <button onClick={() => void execute({ type: "connect" })}>Connect</button>
    </>
  );
}
it("keeps a newer event when an old command response arrives late", async () => {
  let listener: ((s: Snapshot) => void) | undefined;
  let complete: ((s: Snapshot) => void) | undefined;
  const api: BackendClient = {
    snapshot: async () => ({ ...fixture, status: "disconnected" }),
    preview: async () => fixture.servers[0],
    subscribe: async (fn) => {
      listener = fn;
      return () => {};
    },
    execute: () =>
      new Promise((resolve) => {
        complete = resolve;
      }),
    logError: async () => {},
    exportProfile: async () => false,
    inspectProfile: async () => null,
    importProfile: async () => fixture,
  };
  render(
    <BackendProvider client={api}>
      <View />
    </BackendProvider>,
  );
  await screen.findByText("disconnected");
  await userEvent.click(screen.getByRole("button", { name: "Connect" }));
  act(() => listener?.({ ...fixture, status: "connected" }));
  await act(async () => complete?.({ ...fixture, status: "connecting" }));
  expect(screen.getByRole("status")).toHaveTextContent("connected");
});
it("reports the first usable screen once, after the first state", async () => {
  let listener: ((s: Snapshot) => void) | undefined;
  let ready = 0;
  const api: BackendClient = {
    snapshot: async () => ({ ...fixture, status: "disconnected" }),
    preview: async () => fixture.servers[0],
    subscribe: async (fn) => {
      listener = fn;
      return () => {};
    },
    execute: async () => fixture,
    logError: async () => {},
    exportProfile: async () => false,
    inspectProfile: async () => null,
    importProfile: async () => fixture,
    ready: async () => {
      ready += 1;
    },
  };
  render(
    <BackendProvider client={api}>
      <View />
    </BackendProvider>,
  );
  expect(ready).toBe(0);
  await screen.findByText("disconnected");
  await waitFor(() => expect(ready).toBe(1));
  act(() => listener?.({ ...fixture, status: "connected" }));
  await screen.findByText("connected");
  await new Promise((r) => setTimeout(r, 50));
  expect(ready).toBe(1);
});
