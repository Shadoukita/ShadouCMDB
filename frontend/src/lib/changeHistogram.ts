import { currentLocale } from "../i18n/index";

/**
 * The inventory's change histogram (design document §2.7): changes per hour or day to the CIs of
 * the current query, from GET /configuration-items/change-histogram. This file holds what can be
 * tested without a browser: the request for a time range, the bars' geometry and the summary.
 * The API counts; nothing here filters or counts CIs.
 */

export type Bucket = "hour" | "day";
export interface HistogramBucket {
  start: string;
  created: number;
  updated: number;
  statusChanged: number;
}

/** The series, bottom of the stack first. Their colours are --c-viz-1…3 in this order. */
export const SERIES = ["updated", "created", "statusChanged"] as const;
export type Series = (typeof SERIES)[number];

export const RANGES = ["24h", "7d", "30d", "90d"] as const;
export type Range = (typeof RANGES)[number];
export const DEFAULT_RANGE: Range = "7d";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const RANGE_BUCKETS: Record<Range, { bucket: Bucket; n: number }> = {
  "24h": { bucket: "hour", n: 24 },
  "7d": { bucket: "hour", n: 168 },
  "30d": { bucket: "day", n: 30 },
  "90d": { bucket: "day", n: 90 },
};

export interface HistogramWindow {
  from: string;
  /** Unset: up to now (the API's default). */
  to?: string;
  bucket: Bucket;
}

const iso = (ms: number) => new Date(ms).toISOString();

/**
 * The request for a range: `n` whole buckets ending with the current, partial one. `from` is
 * aligned to the bucket (UTC, as the API's buckets are) and `to` is left to the server, so the
 * request, and its cache entry, stay the same until the next bucket begins. The span stays under
 * the API's cap (7 days of hours, 90 of days).
 */
export function rangeWindow(range: Range, now: number): HistogramWindow {
  const { bucket, n } = RANGE_BUCKETS[range];
  const width = bucket === "hour" ? HOUR : DAY;
  const current = Math.floor(now / width) * width;
  return { from: iso(current - (n - 1) * width), bucket };
}

/** One day by hour, after a click on that day's bar. A day that has not ended runs up to now. */
export function dayWindow(dayStart: string, now: number): HistogramWindow {
  const start = Date.parse(dayStart);
  return start + DAY > now ? { from: iso(start), bucket: "hour" } : { from: iso(start), to: iso(start + DAY), bucket: "hour" };
}

export const bucketTotal = (b: HistogramBucket) => b.created + b.updated + b.statusChanged;

/** The busiest bucket (the first of equals), or null when nothing changed. */
export function peak(buckets: readonly HistogramBucket[]): HistogramBucket | null {
  let best: HistogramBucket | null = null;
  for (const b of buckets) if (bucketTotal(b) > 0 && (!best || bucketTotal(b) > bucketTotal(best))) best = b;
  return best;
}

/** The smallest 1, 2 or 5 × 10ⁿ at or above `n`: the top of the scale, so bars compare across ranges. */
export function niceMax(n: number): number {
  if (n <= 1) return 1;
  const p = 10 ** Math.floor(Math.log10(n));
  return ([1, 2, 5, 10].find((m) => m * p >= n) ?? 10) * p;
}

// ---------- Geometry ----------

/** Surface gap between stacked segments and between bars (px). */
export const GAP = 2;
/** Widest bar (px): a bar never fills its slot. */
export const MAX_BAR = 24;
/** Rounded data end (px). */
export const RADIUS = 4;

export interface Segment {
  series: Series;
  y: number;
  height: number;
  /** The top of the bar: drawn with the rounded data end. */
  top: boolean;
}
export interface Column {
  index: number;
  /** The slot: the hover and click target spans all of it. */
  slotX: number;
  slotWidth: number;
  x: number;
  width: number;
  segments: Segment[];
}

/**
 * Stacked bars for `buckets` in a `width` × `height` plot, scaled to `max` (niceMax of the peak).
 * Every non-zero segment is at least 1 px, segments are `GAP` apart, and bars grow from the bottom.
 */
export function layout(buckets: readonly HistogramBucket[], width: number, height: number, max: number): Column[] {
  const n = buckets.length;
  if (n === 0 || width <= 0) return [];
  const slot = width / n;
  const barWidth = Math.max(1, Math.min(MAX_BAR, slot - GAP));
  // Room for the gaps inside the tallest bar, so it still fits.
  const scale = Math.max(0, height - GAP * (SERIES.length - 1)) / max;
  return buckets.map((b, index) => {
    const segments: Segment[] = [];
    let bottom = height;
    for (const series of SERIES) {
      const count = b[series];
      if (count <= 0) continue;
      if (segments.length) bottom -= GAP;
      const h = Math.max(1, count * scale);
      segments.push({ series, y: bottom - h, height: h, top: false });
      bottom -= h;
    }
    if (segments.length) segments[segments.length - 1]!.top = true;
    return { index, slotX: index * slot, slotWidth: slot, x: index * slot + (slot - barWidth) / 2, width: barWidth, segments };
  });
}

/** An SVG path for a bar segment: square at the bottom, rounded at the top when it is the bar's data end. */
export function segmentPath(x: number, y: number, w: number, h: number, rounded: boolean): string {
  const r = rounded ? Math.min(RADIUS, w / 2, h) : 0;
  const f = (v: number) => Number(v.toFixed(2));
  if (r === 0) return `M${f(x)} ${f(y)}h${f(w)}v${f(h)}h${f(-w)}z`;
  return `M${f(x)} ${f(y + h)}v${f(-(h - r))}a${f(r)} ${f(r)} 0 0 1 ${f(r)} ${f(-r)}h${f(w - 2 * r)}a${f(r)} ${f(r)} 0 0 1 ${f(r)} ${f(r)}v${f(h - r)}z`;
}

/** The bucket under a pointer `x` px from the plot's left edge. */
export function indexAt(x: number, width: number, n: number): number {
  if (n === 0 || width <= 0) return -1;
  return Math.min(n - 1, Math.max(0, Math.floor((x / width) * n)));
}

// ---------- Labels ----------

const formats = new Map<string, Intl.DateTimeFormat>();

/**
 * A bucket's start for labels: an hour in the browser's time zone ("Tue 29, 03:00"), a day as the
 * API counts it, in UTC ("Tue, 29 Sep"). German when a test forced the German catalogue, as lib/format does.
 */
export function formatBucket(start: string, bucket: Bucket): string {
  const locale = currentLocale() === "de" ? "de" : undefined;
  const key = `${bucket}\u0000${locale ?? ""}`;
  let f = formats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(
      locale,
      bucket === "hour"
        ? { weekday: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
        : { weekday: "short", day: "numeric", month: "short", timeZone: "UTC" },
    );
    formats.set(key, f);
  }
  const d = new Date(start);
  return Number.isNaN(d.getTime()) ? start : f.format(d);
}

// ---------- Preference ----------

const PREF_KEY = "shadoucmdb.changeHistogram";

/** Whether the strip is open and its range: a per-browser choice, like the density. */
export interface HistogramPref {
  open: boolean;
  range: Range;
}

export function readPref(storage: Pick<Storage, "getItem"> | undefined = globalThis.localStorage): HistogramPref {
  try {
    const raw = storage?.getItem(PREF_KEY);
    if (raw === "closed") return { open: false, range: DEFAULT_RANGE };
    if ((RANGES as readonly string[]).includes(raw ?? "")) return { open: true, range: raw as Range };
  } catch {
    // Storage disabled: the defaults.
  }
  return { open: true, range: DEFAULT_RANGE };
}

export function writePref(pref: HistogramPref, storage: Pick<Storage, "setItem" | "removeItem"> | undefined = globalThis.localStorage): void {
  try {
    if (!pref.open) storage?.setItem(PREF_KEY, "closed");
    else if (pref.range === DEFAULT_RANGE) storage?.removeItem(PREF_KEY);
    else storage?.setItem(PREF_KEY, pref.range);
  } catch {
    // Storage disabled: the choice lasts for this page load.
  }
}
