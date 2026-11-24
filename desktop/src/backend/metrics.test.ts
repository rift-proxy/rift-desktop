import { expect, it, vi } from "vitest";
import { IpcMetrics, measured, measuredListener, percentile } from "./metrics";

it("nearest-rank percentiles", () => {
  const sorted = Array.from({ length: 100 }, (_, i) => i + 1);
  expect(percentile(sorted, 50)).toBe(50);
  expect(percentile(sorted, 95)).toBe(95);
  expect(percentile(sorted, 99)).toBe(99);
  expect(percentile([7], 99)).toBe(7);
  expect(percentile([1, 2], 0)).toBe(1);
});

it("keeps the total count and max but percentiles of the recent 512 calls", () => {
  const m = new IpcMetrics();
  m.observe("snapshot", 1000);
  for (let i = 0; i < 512; i++) m.observe("snapshot", 2);
  expect(m.report().snapshot).toEqual({
    count: 513,
    p50: 2,
    p95: 2,
    p99: 2,
    max: 1000,
  });
  m.reset();
  expect(m.report()).toEqual({});
});

it("times calls end to end, including failures", async () => {
  const m = new IpcMetrics();
  const now = vi.spyOn(performance, "now");
  now.mockReturnValueOnce(10).mockReturnValueOnce(35.04);
  await expect(measured("execute:connect", async () => "ok", m)).resolves.toBe(
    "ok",
  );
  now.mockReturnValueOnce(0).mockReturnValueOnce(5);
  await expect(
    measured("execute:connect", () => Promise.reject(new Error("x")), m),
  ).rejects.toThrow("x");
  now.mockRestore();
  expect(m.report()["execute:connect"]).toMatchObject({ count: 2, max: 25 });
});

it("counts events and passes each payload to the listener", () => {
  const m = new IpcMetrics();
  const seen: number[] = [];
  const listener = measuredListener<number>(
    "event:rift://state",
    (n) => seen.push(n),
    m,
  );
  listener(1);
  listener(2);
  expect(seen).toEqual([1, 2]);
  expect(m.report()["event:rift://state"].count).toBe(2);
});
