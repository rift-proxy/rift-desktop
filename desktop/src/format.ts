import { localeTag, t, tFor } from "./i18n";

export function latency(value: number | null): string {
  return value === null ? "—" : t("format.latency", { value });
}
export function time(value: number | null): string {
  return value === null
    ? t("format.not_checked")
    : new Date(value * 1000).toLocaleTimeString(localeTag());
}
/** When a server was checked: the time of day for today, with the date for an earlier day (the
 * list is ordered by the whole moment, so what it shows has to carry the day as well). */
export function checkedAt(
  value: number | null,
  now: number = Date.now(),
): string {
  if (value === null) return t("format.not_checked");
  const at = new Date(value * 1000);
  const clock = at.toLocaleTimeString(localeTag());
  if (at.toDateString() === new Date(now).toDateString()) return clock;
  const day = at.toLocaleDateString(localeTag(), {
    day: "2-digit",
    month: "2-digit",
  });
  return `${day} ${clock}`;
}
/** When a server that did not answer is asked again; nothing for one that is measured as usual. */
export function retryText(at: number | null | undefined): string | null {
  return at === null || at === undefined
    ? null
    : t("format.retry_at", {
        time: new Date(at * 1000).toLocaleTimeString(localeTag(), {
          hour: "2-digit",
          minute: "2-digit",
        }),
      });
}
export function countdown(deadline: number | null, now: number): string {
  return deadline === null
    ? "—"
    : t("format.countdown", { value: Math.max(0, Math.ceil(deadline - now)) });
}
/** One name for a mode everywhere it is shown (connection page, settings). */
export function modeLabel(mode: string): string {
  return tFor("mode", mode) ?? mode;
}
export function connectionLabel(status: string): string {
  return tFor("connection", status) ?? t("connection.unknown");
}
/** Failure codes are internal. Unknown future values always get a safe localized fallback.
 * No code, `null` or an absent key (Rust omits it for ordinary journal entries), means no
 * failure. */
export function failureText(
  code: string | null | undefined,
  unknown = t("failure.unknown"),
): string | null {
  if (code === null || code === undefined) return null;
  return (code !== "unknown" ? tFor("failure", code) : null) ?? unknown;
}

const BYTE_UNITS = ["b", "kb", "mb", "gb", "tb", "pb"];
/** A size in bytes with a Russian binary unit: `1,5 ГБ`. */
export function bytes(value: number): string {
  let size = Math.max(0, value);
  let unit = 0;
  while (size >= 1024 && unit < BYTE_UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  // Stryker disable next-line EqualityOperator: exactly 100 reads "100" with either comparison
  const whole = unit === 0 || size >= 100;
  const text = whole
    ? String(Math.round(size))
    : size.toFixed(1).replace(".0", "").replace(".", ",");
  return t("format.size", {
    value: text,
    unit: t(`bytes.${BYTE_UNITS[unit]}`),
  });
}
/** A speed in bytes per second: `1,2 МБ/с`. */
export function speedText(bytesPerSecond: number): string {
  return t("format.speed", { value: bytes(bytesPerSecond) });
}
/** How long something has lasted: `5 с`, `2 мин 10 с`, `1 ч 5 мин`. */
export function durationText(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  if (total < 60) return t("format.duration.seconds", { s: total });
  const minutes = Math.floor(total / 60);
  if (total < 3600) {
    const rest = total % 60;
    return rest === 0
      ? t("format.duration.minutes", { m: minutes })
      : t("format.duration.minutes_seconds", { m: minutes, s: rest });
  }
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0
    ? t("format.duration.hours", { h: hours })
    : t("format.duration.hours_minutes", { h: hours, m: rest });
}
/** A date for a term: `18.09.2026`. */
export function dateText(unix: number): string {
  return new Date(unix * 1000).toLocaleDateString(localeTag());
}
/** A moment with its date, as the journal shows it: `21.10.2026 14:03:07`. */
export function dateTime(unix: number): string {
  return `${dateText(unix)} ${time(unix)}`;
}
/** What the tip of a status says: when the status was set, with the day, or that it never was. */
export function statusTitle(checked: number | null): string {
  return checked === null
    ? t("format.never_checked")
    : t("format.checked_at", { at: dateTime(checked) });
}
/** What a provider reports about traffic: `40 МБ из 100 ГБ`, or only what was used. */
export function usageText(usage: {
  used: number;
  total: number;
}): string | null {
  if (usage.total > 0)
    return t("format.usage_of", {
      used: bytes(usage.used),
      total: bytes(usage.total),
    });
  return usage.used > 0
    ? t("format.usage_used", { used: bytes(usage.used) })
    : null;
}
/** Why Auto Select skips a subscription, or `null` when it does not. */
export function availabilityText(availability: string): string | null {
  return tFor("availability", availability);
}
