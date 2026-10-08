// The dashboard's period and figures (SHAA-1670 §0 step 12e): requests per period, deltas and mini bars.
// Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  barHeights,
  colourByRank,
  changeWindows,
  completePercent,
  countDelta,
  countWindow,
  group,
  parsePeriod,
  partOfDay,
  percentChange,
} from "../src/lib/dashboard";


const NOW = Date.parse("2026-10-07T09:42:13Z");

describe("period", () => {
  test("parsePeriod accepts the three periods and defaults to 14 days", () => {
    assert.equal(parsePeriod("24h"), "24h");
    assert.equal(parsePeriod("90d"), "90d");
    assert.equal(parsePeriod("7d"), "14d");
    assert.equal(parsePeriod(undefined), "14d");
    assert.equal(parsePeriod(["24h"]), "14d");
  });

  test("changeWindows: whole UTC buckets ending with the current one, and as many just before", () => {
    assert.deepEqual(changeWindows("24h", NOW), {
      current: { from: "2026-10-06T10:00:00.000Z", bucket: "hour" },
      previous: { from: "2026-10-05T10:00:00.000Z", to: "2026-10-06T10:00:00.000Z", bucket: "hour" },
    });
    assert.deepEqual(changeWindows("14d", NOW), {
      current: { from: "2026-09-24T00:00:00.000Z", bucket: "day" },
      previous: { from: "2026-09-10T00:00:00.000Z", to: "2026-09-24T00:00:00.000Z", bucket: "day" },
    });
    const ninety = changeWindows("90d", NOW);
    assert.equal(ninety.current.from, "2026-07-10T00:00:00.000Z");
    // The API's cap for day buckets is 90 days: the previous window is exactly that long.
    assert.equal(Date.parse(ninety.previous.to!) - Date.parse(ninety.previous.from), 90 * 86_400_000);
  });

  test("countWindow: 14 days by day, 90 days by week", () => {
    assert.deepEqual(countWindow("24h", NOW), { from: "2026-09-24T00:00:00.000Z", bucket: "day" });
    assert.deepEqual(countWindow("14d", NOW), { from: "2026-09-24T00:00:00.000Z", bucket: "day" });
    assert.deepEqual(countWindow("90d", NOW), { from: "2026-07-09T00:00:00.000Z", bucket: "week" });
  });
});

describe("figures", () => {
  const h = {
    from: "2026-10-05T00:00:00Z",
    countAtFrom: 100,
    buckets: [
      { start: "2026-10-05T00:00:00Z", count: 104 },
      { start: "2026-10-06T00:00:00Z", count: 110 },
      { start: "2026-10-07T00:00:00Z", count: 107 },
    ],
  };
  test("countDelta: since the history's start, or since today for 24 h", () => {
    assert.deepEqual(countDelta(h, "14d"), { value: 107, delta: 7, since: "2026-10-05T00:00:00Z" });
    assert.deepEqual(countDelta(h, "24h"), { value: 107, delta: -3, since: "2026-10-07T00:00:00Z" });
    assert.deepEqual(countDelta({ ...h, buckets: [h.buckets[0]!] }, "24h"), { value: 104, delta: 4, since: "2026-10-05T00:00:00Z" });
    assert.equal(countDelta({ ...h, buckets: [] }, "14d"), null);
  });

  test("percentChange: none against an empty period", () => {
    assert.equal(percentChange(118, 100), 18);
    assert.equal(percentChange(50, 100), -50);
    assert.equal(percentChange(5, 0), null);
  });

  test("group sums neighbours into at most 24 bars", () => {
    assert.deepEqual(group([1, 2, 3]), [1, 2, 3]);
    const ninety = group(Array.from({ length: 90 }, () => 1));
    assert.ok(ninety.length <= 24);
    assert.equal(ninety.reduce((a, b) => a + b, 0), 90);
  });

  test("barHeights: counts span their range, changes grow from zero, a non-zero value stays visible", () => {
    assert.deepEqual(barHeights([10, 20, 30], "range"), [30, 65, 100]);
    assert.deepEqual(barHeights([5, 5], "range"), [65, 65]);
    assert.deepEqual(barHeights([0, 1, 100], "zero"), [0, 6, 100]);
    assert.deepEqual(barHeights([0, 0], "zero"), [0, 0]);
    assert.deepEqual(barHeights([], "zero"), []);
  });

  test("completePercent: one decimal; nothing to count is complete", () => {
    assert.equal(completePercent(1000, 942), 94.2);
    assert.equal(completePercent(3, 1), 33.3);
    assert.equal(completePercent(0, 0), 100);
  });

  test("partOfDay", () => {
    assert.equal(partOfDay(4), "evening");
    assert.equal(partOfDay(5), "morning");
    assert.equal(partOfDay(12), "afternoon");
    assert.equal(partOfDay(18), "evening");
  });

  test("class colours by rank: six series, then grey", () => {
    assert.deepEqual([0, 1, 2, 3, 4, 5, 6, 9].map(colourByRank), [1, 4, 2, 5, 6, 7, 8, 8]);
  });
});
