import { currentLocale, t } from "../i18n/index";

// The browser's locale, unless a test forced the German catalog: then German dates too (GH#478).
const formatters = new Map<string, Intl.DateTimeFormat>();
function formatter(kind: "dateTime" | "dateOnly"): Intl.DateTimeFormat {
  const forced = currentLocale() === "de" ? "de" : undefined;
  const key = `${kind}\u0000${forced ?? ""}`;
  let f = formatters.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(forced, kind === "dateTime" ? { dateStyle: "medium", timeStyle: "short" } : { dateStyle: "medium" });
    formatters.set(key, f);
  }
  return f;
}

/** Placeholder for a reference into a class the caller may not view (the API withholds its name). */
export const HIDDEN_CI = "Hidden CI";

export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : formatter("dateTime").format(d);
}

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return "";
  // Plain dates ("2025-03-01") are calendar dates; do not shift them through the local timezone.
  const d = /^\d{4}-\d{2}-\d{2}$/.test(iso) ? new Date(`${iso}T00:00:00`) : new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : formatter("dateOnly").format(d);
}

export function formatRelative(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const min = Math.round(diff / 60_000);
  if (min < 1) return t("time.justNow");
  if (min < 60) return t("time.minutesAgo", { n: min });
  const h = Math.round(min / 60);
  if (h < 24) return t("time.hoursAgo", { n: h });
  const d = Math.round(h / 24);
  if (d < 30) return t("time.daysAgo", { n: d });
  return formatDate(iso);
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}

/** File sizes as operators read them: "23.4 MB", "512 KB" (binary units, as the server's limits are). */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n.toLocaleString()} bytes`;
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toLocaleString(undefined, { maximumFractionDigits: v < 10 ? 1 : 0 })} ${units[i]}`;
}

/**
 * A name that reads as a hostname or FQDN (`fra1-esx-01`, `db01.example.com`): one DNS-style token with a digit,
 * hyphen or dot in it. Such a record title is set in mono (design §2.2); a plain word or a phrase is not.
 */
export function isHostLike(name: string): boolean {
  return /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,62})(?:\.[A-Za-z0-9-]{1,63})*$/.test(name) && /[0-9.-]/.test(name);
}
