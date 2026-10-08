// Unit tests for the inventory's CSV export parameters (SHAA-2535). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { exportFileName, exportUnsupported, inventoryExportQuery } from "../src/lib/inventoryExport";

const CLASS = "11111111-1111-4111-8111-111111111111";

describe("inventory export query", () => {
  test("keeps the filters and sort, drops the page, adds the columns in order", () => {
    const list = { classId: CLASS, q: "web", active: "all", lookupValueId: "a,b", sort: "-updatedAt", limit: 50, offset: 100, ownLayout: undefined };
    assert.deepEqual(inventoryExportQuery(list, ["label", "attributes.hostname", "class"], "comma"), {
      classId: CLASS,
      q: "web",
      active: "all",
      lookupValueId: "a,b",
      sort: "-updatedAt",
      columns: "label,attributes.hostname,class",
    });
  });
  test("sends the delimiter only when it is not the default", () => {
    assert.equal(inventoryExportQuery({}, ["label"], "comma").delimiter, undefined);
    assert.equal(inventoryExportQuery({}, ["label"], "semicolon").delimiter, "semicolon");
  });
  test("leaves the data-quality drill-down out and flags it", () => {
    const list = { quality: "missingOwner", endOfLifeWithinDays: 90, limit: 50 };
    assert.deepEqual(inventoryExportQuery(list, [], "comma"), {});
    assert.equal(exportUnsupported(list), true);
    assert.equal(exportUnsupported({ quality: undefined, classId: CLASS }), false);
  });
  test("file name", () => {
    assert.equal(exportFileName("server", "20261008-1432"), "inventory-server-20261008-1432.csv");
    assert.equal(exportFileName(undefined, "20261008-1432"), "inventory-all-20261008-1432.csv");
  });
});
