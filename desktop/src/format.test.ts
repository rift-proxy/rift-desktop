import { describe, it, expect } from "vitest";
import {
  availabilityText,
  bytes,
  checkedAt,
  dateText,
  dateTime,
  usageText,
  latency,
  countdown,
  connectionLabel,
  speedText,
  statusTitle,
  durationText,
  failureText,
  retryText,
  time,
  modeLabel,
} from "./format";
describe("presentation formatting", () => {
  it("distinguishes unknown from measured zero", () => {
    expect(latency(null)).toBe("—");
    expect(latency(0)).toBe("0 мс");
    expect(latency(64)).toBe("64 мс");
  });
  it("rounds countdown upward and clamps elapsed time", () => {
    expect(countdown(null, 50)).toBe("—");
    expect(countdown(60, 50.1)).toBe("10 сек");
    expect(countdown(40, 50)).toBe("0 сек");
    expect(countdown(50, 50)).toBe("0 сек");
  });
  it("labels each connection state and unknown", () => {
    expect(connectionLabel("connected")).toBe("Подключено");
    expect(connectionLabel("connecting")).toBe("Подключение…");
    expect(connectionLabel("disconnected")).toBe("Отключено");
    expect(connectionLabel("error")).toBe("Ошибка подключения");
    expect(connectionLabel("lost")).toBe("Соединение потеряно");
    expect(connectionLabel("unknown")).toBe("Неизвестное состояние");
    // A state the catalog has no text for gets the same fallback, not the code or an empty text.
    expect(connectionLabel("state_of_a_later_version")).toBe(
      "Неизвестное состояние",
    );
    expect(time(null)).toBe("Ещё не проверено");
    const stamp = new Date(2026, 8, 19, 5, 7, 9).getTime() / 1000;
    expect(time(stamp)).toBe("05:07:09");
  });
  it("shows when a server was checked: the time today, the date and time before", () => {
    const stamp = new Date(2026, 8, 5, 5, 7, 9).getTime() / 1000;
    const sameDay = new Date(2026, 8, 5, 23, 59, 0).getTime();
    expect(checkedAt(null, sameDay)).toBe("Ещё не проверено");
    expect(checkedAt(stamp, sameDay)).toBe("05:07:09");
    // Another day of the same month, of another month, of another year: the date shows.
    expect(checkedAt(stamp, new Date(2026, 8, 6, 0, 1, 0).getTime())).toBe(
      "05.09 05:07:09",
    );
    expect(checkedAt(stamp, new Date(2026, 9, 5, 5, 7, 9).getTime())).toBe(
      "05.09 05:07:09",
    );
    expect(checkedAt(stamp, new Date(2027, 8, 5, 5, 7, 9).getTime())).toBe(
      "05.09 05:07:09",
    );
    // Right now is today.
    expect(checkedAt(Math.floor(Date.now() / 1000))).toMatch(
      /^\d{2}:\d{2}:\d{2}$/,
    );
  });
  it("says when a server that did not answer is asked again, and nothing for the others", () => {
    expect(retryText(null)).toBeNull();
    expect(retryText(undefined)).toBeNull();
    const morning = new Date(2026, 8, 19, 5, 7, 9).getTime() / 1000;
    expect(retryText(morning)).toBe("Спросим снова в 05:07");
    const afternoon = new Date(2026, 8, 19, 14, 30, 59).getTime() / 1000;
    expect(retryText(afternoon)).toBe("Спросим снова в 14:30");
    // The start of the epoch is a time like any other, not "nothing".
    expect(retryText(0)).toMatch(/^Спросим снова в \d{2}:\d{2}$/);
  });
  it("localizes failure codes without exposing unknown technical values", () => {
    expect(failureText("endpoint_timeout")).toBe("Сервер не ответил вовремя");
    expect(failureText("future_dependency_detail")).toBe(
      "Не удалось выполнить операцию",
    );
    expect(failureText("future_dependency_detail", "Сбой проверки")).toBe(
      "Сбой проверки",
    );
    // The code "unknown" is never looked up: it gets the fallback, the caller's own one included.
    expect(failureText("unknown")).toBe("Не удалось выполнить операцию");
    expect(failureText("unknown", "Сбой проверки")).toBe("Сбой проверки");
    expect(failureText(null)).toBeNull();
    // Rust omits the key of an ordinary journal entry: that is "no failure", not "unknown".
    expect(failureText(undefined)).toBeNull();
    expect(failureText(undefined, "Сбой проверки")).toBeNull();
  });
  it("names a mode the same way everywhere", () => {
    expect(modeLabel("proxy")).toBe("Proxy");
    expect(modeLabel("tun")).toBe("TUN");
    expect(modeLabel("tun_proxy")).toBe("TUN + Proxy");
    expect(modeLabel("future")).toBe("future");
  });
});

describe("what a provider reports", () => {
  it("writes sizes with binary Russian units", () => {
    expect(bytes(0)).toBe("0 Б");
    expect(bytes(1023)).toBe("1023 Б");
    expect(bytes(1024)).toBe("1 КБ");
    expect(bytes(1536)).toBe("1,5 КБ");
    expect(bytes(40 * 1024 ** 2)).toBe("40 МБ");
    expect(durationText(0)).toBe("0 с");
    expect(durationText(59.9)).toBe("59 с");
    expect(durationText(60)).toBe("1 мин");
    expect(durationText(130)).toBe("2 мин 10 с");
    expect(durationText(3599)).toBe("59 мин 59 с");
    expect(durationText(3600)).toBe("1 ч");
    expect(durationText(3900)).toBe("1 ч 5 мин");
    expect(durationText(-5)).toBe("0 с");
    expect(speedText(0)).toBe("0 Б/с");
    expect(speedText(1536)).toBe("1,5 КБ/с");
    expect(speedText(3 * 1024 ** 2)).toBe("3 МБ/с");
    expect(bytes(1073741824000)).toBe("1000 ГБ");
    expect(bytes(150 * 1024 ** 3)).toBe("150 ГБ");
    expect(bytes(-5)).toBe("0 Б");
    expect(bytes(Number.MAX_SAFE_INTEGER)).toMatch(/ ПБ$/);
    // Every unit, the last one included, and no unit past it.
    expect(bytes(2 * 1024 ** 4)).toBe("2 ТБ");
    expect(bytes(3 * 1024 ** 5)).toBe("3 ПБ");
    expect(bytes(2 ** 60)).toBe("1024 ПБ");
    // A fraction of a byte is rounded, not written with a comma; 100 and more are whole numbers.
    expect(bytes(5.4)).toBe("5 Б");
    expect(bytes(0.4)).toBe("0 Б");
    expect(bytes(150.5 * 1024 ** 3)).toBe("151 ГБ");
    expect(bytes(99.4 * 1024 ** 2)).toBe("99,4 МБ");
  });
  it("describes quota or only use, and nothing when there is none", () => {
    expect(usageText({ used: 40 * 1024 ** 2, total: 100 * 1024 ** 3 })).toBe(
      "40 МБ из 100 ГБ",
    );
    expect(usageText({ used: 1536, total: 0 })).toBe("Использовано 1,5 КБ");
    expect(usageText({ used: 0, total: 0 })).toBeNull();
  });
  it("names the reason Auto Select skips a subscription", () => {
    expect(availabilityText("ok")).toBeNull();
    expect(availabilityText("exhausted")).toMatch(/Трафик исчерпан/);
    expect(availabilityText("expired")).toMatch(/Срок истёк/);
    expect(availabilityText("future")).toBeNull();
  });
  it("writes a term as a local date", () => {
    expect(dateText(new Date(2027, 0, 15, 12).getTime() / 1000)).toBe(
      "15.01.2027",
    );
  });
  it("writes a moment with its date and no comma between them", () => {
    const stamp = new Date(2026, 9, 21, 14, 3, 7).getTime() / 1000;
    expect(dateTime(stamp)).toBe("21.10.2026 14:03:07");
    expect(dateTime(new Date(2027, 0, 5, 0, 0, 0).getTime() / 1000)).toBe(
      "05.01.2027 00:00:00",
    );
  });
  it("names the moment a status was set, or says it never was", () => {
    const stamp = new Date(2026, 9, 21, 14, 3, 7).getTime() / 1000;
    expect(statusTitle(stamp)).toBe("Проверено 21.10.2026 14:03:07");
    expect(statusTitle(null)).toBe("Ещё не проверялся");
  });
});
