// The inventory's change histogram (SHAA-1670 rollout 5d): the request for a range, the bars'
// geometry and the stored preference. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  GAP,
  MAX_BAR,
  dayWindow,
  indexAt,
  layout,
  niceMax,
  peak,
  rangeWindow,
  readPref,
  segmentPath,
  writePref,
  type HistogramBucket,
} from "../src/lib/changeHistogram";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const NOW = Date.parse("2026-10-04T02:37:12Z");
const b = (start: string, updated = 0, created = 0, statusChanged = 0): HistogramBucket => ({ start, updated, created, statusChanged });

describe("rangeWindow", () => {
  test("hour ranges end with the current hour and stay under the API's 7-day cap", () => {
    assert.deepEqual(rangeWindow("24h", NOW), { from: "2026-10-03T03:00:00.000Z", bucket: "hour" });
    const week = rangeWindow("7d", NOW);
    assert.equal(week.bucket, "hour");
    assert.equal(week.from, "2026-09-27T03:00:00.000Z");
    assert.ok(NOW - Date.parse(week.from) < 7 * DAY);
  });

  test("day ranges are UTC days ending with today and stay under the 90-day cap", () => {
    assert.deepEqual(rangeWindow("30d", NOW), { from: "2026-09-05T00:00:00.000Z", bucket: "day" });
    const quarter = rangeWindow("90d", NOW);
    assert.equal(quarter.from, "2026-07-07T00:00:00.000Z");
    assert.ok(NOW - Date.parse(quarter.from) < 90 * DAY);
  });

  test("the request does not change within the hour, so its cache entry holds", () => {
    assert.deepEqual(rangeWindow("7d", NOW), rangeWindow("7d", NOW + 20 * 60_000));
    assert.notDeepEqual(rangeWindow("7d", NOW), rangeWindow("7d", NOW + HOUR));
  });
});

describe("dayWindow", () => {
  test("a past day is its 24 hours", () => {
    assert.deepEqual(dayWindow("2026-10-01T00:00:00Z", NOW), { from: "2026-10-01T00:00:00.000Z", to: "2026-10-02T00:00:00.000Z", bucket: "hour" });
  });
  test("today runs up to now (no `to` in the future)", () => {
    assert.deepEqual(dayWindow("2026-10-04T00:00:00Z", NOW), { from: "2026-10-04T00:00:00.000Z", bucket: "hour" });
  });
});

test("niceMax rounds up to 1, 2 or 5 × 10ⁿ", () => {
  assert.deepEqual([0, 1, 2, 3, 7, 10, 11, 71, 200, 201, 4999].map(niceMax), [1, 1, 2, 5, 10, 10, 20, 100, 200, 500, 5000]);
});

test("peak is the first busiest bucket, or null when nothing changed", () => {
  assert.equal(peak([b("a"), b("b")]), null);
  assert.equal(peak([b("a", 1), b("b", 0, 3), b("c", 2, 1)])?.start, "b");
});

describe("layout", () => {
  const buckets = [b("a", 10, 5, 5), b("b"), b("c", 0, 1)];

  test("bars are centred in their slot, at most MAX_BAR wide, with a gap between them", () => {
    const wide = layout(buckets, 300, 80, 20);
    assert.equal(wide[0]!.width, MAX_BAR);
    assert.equal(wide[0]!.x, (100 - MAX_BAR) / 2);
    const narrow = layout(buckets, 30, 80, 20);
    assert.equal(narrow[0]!.width, 10 - GAP);
    assert.equal(layout(Array.from({ length: 90 }, () => b("x", 1)), 90, 80, 1)[0]!.width, 1);
  });

  test("segments stack from the baseline in series order, a gap apart, and only the top one is rounded", () => {
    const [col] = layout(buckets, 300, 80, 20);
    assert.deepEqual(
      col!.segments.map((s) => s.series),
      ["updated", "created", "statusChanged"],
    );
    const [u, c, st] = col!.segments;
    assert.equal(u!.y + u!.height, 80);
    assert.equal(c!.y + c!.height, u!.y - GAP);
    assert.equal(st!.y + st!.height, c!.y - GAP);
    assert.ok(st!.y >= 0, "the tallest bar fits, gaps included");
    assert.deepEqual(
      col!.segments.map((s) => s.top),
      [false, false, true],
    );
  });

  test("an empty bucket has no segments; a small count is still visible", () => {
    const cols = layout([b("a", 1000), b("b"), b("c", 1)], 300, 80, 1000);
    assert.equal(cols[1]!.segments.length, 0);
    assert.ok(cols[2]!.segments[0]!.height >= 1);
  });

  test("nothing to draw without width or buckets", () => {
    assert.deepEqual(layout([], 300, 80, 1), []);
    assert.deepEqual(layout(buckets, 0, 80, 1), []);
  });
});

test("segmentPath rounds the data end only, never past half the bar", () => {
  assert.equal(segmentPath(0, 10, 8, 20, false), "M0 10h8v20h-8z");
  assert.equal(segmentPath(0, 10, 8, 20, true), "M0 30v-16a4 4 0 0 1 4 -4h0a4 4 0 0 1 4 4v16z");
  assert.match(segmentPath(0, 0, 2, 20, true), /a1 1 /);
});

test("indexAt maps the pointer to a bucket and clamps at the edges", () => {
  assert.equal(indexAt(0, 300, 3), 0);
  assert.equal(indexAt(150, 300, 3), 1);
  assert.equal(indexAt(300, 300, 3), 2);
  assert.equal(indexAt(-5, 300, 3), 0);
  assert.equal(indexAt(10, 300, 0), -1);
});

describe("preference", () => {
  const store = () => {
    const m = new Map<string, string>();
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k), m };
  };

  test("open on 7 days by default; the default is not stored", () => {
    const s = store();
    assert.deepEqual(readPref(s), { open: true, range: "7d" });
    writePref({ open: true, range: "7d" }, s);
    assert.equal(s.m.size, 0);
  });

  test("a range and a closed strip survive a reload; junk reads as the default", () => {
    const s = store();
    writePref({ open: true, range: "30d" }, s);
    assert.deepEqual(readPref(s), { open: true, range: "30d" });
    writePref({ open: false, range: "30d" }, s);
    assert.equal(readPref(s).open, false);
    s.setItem("shadoucmdb.changeHistogram", "1y");
    assert.deepEqual(readPref(s), { open: true, range: "7d" });
  });

  test("storage that throws reads as the default and writes nothing", () => {
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {
        throw new Error("denied");
      },
    };
    assert.deepEqual(readPref(broken), { open: true, range: "7d" });
    assert.doesNotThrow(() => writePref({ open: false, range: "7d" }, broken));
  });
});
