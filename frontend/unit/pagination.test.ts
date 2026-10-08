import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { meterFill, pageItems } from "../src/lib/pagination";

describe("pageItems", () => {
  test("shows the first pages, a gap and the last page near the start", () => {
    assert.deepEqual(pageItems(1, 193), [1, 2, 3, "gap", 193]);
    assert.deepEqual(pageItems(3, 193), [1, 2, 3, 4, "gap", 193]);
  });
  test("keeps the current page with one neighbour each side in the middle", () => {
    assert.deepEqual(pageItems(50, 193), [1, "gap", 49, 50, 51, "gap", 193]);
  });
  test("shows a single hidden page instead of a gap", () => {
    assert.deepEqual(pageItems(4, 10), [1, 2, 3, 4, 5, "gap", 10]);
  });
  test("shows the last pages near the end", () => {
    assert.deepEqual(pageItems(193, 193), [1, "gap", 191, 192, 193]);
  });
  test("lists every page when there are few", () => {
    assert.deepEqual(pageItems(1, 1), [1]);
    assert.deepEqual(pageItems(2, 4), [1, 2, 3, 4]);
    assert.deepEqual(pageItems(1, 0), []);
  });
});

describe("meterFill", () => {
  test("fills every bar for rank 1 and one bar for the lowest rank", () => {
    assert.equal(meterFill(1, 4), 4);
    assert.equal(meterFill(2, 4), 3);
    assert.equal(meterFill(4, 4), 1);
  });
  test("stays within the meter", () => {
    assert.equal(meterFill(9, 4), 1);
    assert.equal(meterFill(0, 4), 4);
    assert.equal(meterFill(1, 0), 0);
  });
});
