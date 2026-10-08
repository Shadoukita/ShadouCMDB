/**
 * The dashboard's period and its figures (design document §0 step 12e): the requests for a period and what
 * can be tested without a browser, the deltas and the mini bars. The API counts; nothing here counts CIs.
 */

export const PERIODS = ["24h", "14d", "90d"] as const;
export type Period = (typeof PERIODS)[number];
export const DEFAULT_PERIOD: Period = "14d";

/** A `?period=` from the URL, or the default. */
export function parsePeriod(raw: unknown): Period {
  return (PERIODS as readonly unknown[]).includes(raw) ? (raw as Period) : DEFAULT_PERIOD;
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const iso = (ms: number) => new Date(ms).toISOString();

export interface ChangeWindow {
  from: string;
  to?: string;
  bucket: "hour" | "day";
}

const CHANGE_BUCKETS: Record<Period, { bucket: "hour" | "day"; n: number }> = {
  "24h": { bucket: "hour", n: 24 },
  "14d": { bucket: "day", n: 14 },
  "90d": { bucket: "day", n: 90 },
};

/**
 * The change histogram's request for a period: `n` whole UTC buckets ending with the current, partial one
 * (`to` left to the server, so the cache entry holds until the next bucket begins), and the same number of
 * buckets just before it, for the comparison.
 */
export function changeWindows(period: Period, now: number): { current: ChangeWindow; previous: ChangeWindow } {
  const { bucket, n } = CHANGE_BUCKETS[period];
  const width = bucket === "hour" ? HOUR : DAY;
  const start = Math.floor(now / width) * width - (n - 1) * width;
  return {
    current: { from: iso(start), bucket },
    previous: { from: iso(start - n * width), to: iso(start), bucket },
  };
}

export interface CountWindow {
  from: string;
  bucket: "day" | "week";
}

/** Bars of a count KPI: 14 UTC days for 24 h and 14 days, 13 ISO weeks for 90 days (the API rounds to Monday). */
export function countWindow(period: Period, now: number): CountWindow {
  const today = Math.floor(now / DAY) * DAY;
  return period === "90d" ? { from: iso(today - 90 * DAY), bucket: "week" } : { from: iso(today - 13 * DAY), bucket: "day" };
}

export interface CountHistoryLike {
  from: string;
  countAtFrom: number;
  buckets: readonly { start: string; count: number }[];
}

/**
 * A count now, and how it moved over the period. For 24 h that is since the start of today's UTC day (the
 * history counts per day), otherwise since the start of the history. `since` names the start for the label.
 */
export function countDelta(h: CountHistoryLike, period: Period): { value: number; delta: number; since: string } | null {
  const last = h.buckets[h.buckets.length - 1];
  if (!last) return null;
  if (period === "24h") {
    const before = h.buckets.length > 1 ? h.buckets[h.buckets.length - 2]!.count : h.countAtFrom;
    return { value: last.count, delta: last.count - before, since: last.start };
  }
  return { value: last.count, delta: last.count - h.countAtFrom, since: h.from };
}

/** Change against the previous period in whole percent, or null when that had none. */
export function percentChange(current: number, previous: number): number | null {
  return previous > 0 ? Math.round(((current - previous) / previous) * 100) : null;
}

/** At most this many mini bars per KPI card. */
export const MAX_BARS = 24;

/** Sums neighbouring values into at most `max` groups (the last group may be shorter). */
export function group(values: readonly number[], max = MAX_BARS): number[] {
  if (values.length <= max) return [...values];
  const size = Math.ceil(values.length / max);
  const out: number[] = [];
  for (let i = 0; i < values.length; i += size) out.push(values.slice(i, i + size).reduce((a, b) => a + b, 0));
  return out;
}

/**
 * Heights in percent of the bar area. Counts (`baseline: "range"`) move little against their size, so their
 * bars span the period's low to high over 30–100 %; changes grow from zero. A value above 0 is always visible.
 */
export function barHeights(values: readonly number[], baseline: "zero" | "range"): number[] {
  if (values.length === 0) return [];
  const max = Math.max(...values);
  if (baseline === "zero") return values.map((v) => (max > 0 && v > 0 ? Math.max(6, Math.round((v / max) * 100)) : 0));
  const min = Math.min(...values);
  return values.map((v) => (max === min ? 65 : Math.round(30 + ((v - min) / (max - min)) * 70)));
}

/** Records complete in percent with one decimal; an empty selection is complete. */
export function completePercent(items: number, complete: number): number {
  return items > 0 ? Math.round((complete / items) * 1000) / 10 : 100;
}

/** "Good morning", "Good afternoon" or "Good evening" by the hour in the browser's time zone. */
export function partOfDay(hour: number): "morning" | "afternoon" | "evening" {
  return hour >= 5 && hour < 12 ? "morning" : hour >= 12 && hour < 18 ? "afternoon" : "evening";
}

/**
 * Categorical colours of the dashboard's bars and class dots (design §0.1): --c-viz-1, 4, 2, 5, 6, 7 by rank,
 * the grey --c-viz-8 for the rest. Text never takes a series colour; the label beside it is the reading.
 */
export const RANK_COLOURS = [1, 4, 2, 5, 6, 7] as const;
export const OTHER_COLOUR = 8;

export const colourByRank = (rank: number): number => RANK_COLOURS[rank] ?? OTHER_COLOUR;
