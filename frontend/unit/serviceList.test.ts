// Unit tests for the business service list's URL state (SHAA-927 §5.2). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  DEFAULT_SERVICE_LIST,
  hasServiceFilters,
  ownerCell,
  parseServiceListQuery,
  serviceListParams,
  serviceListUrl,
} from "../src/lib/serviceList";

const A = "0b7c4a3e-1d2f-4c5b-9a8e-7f6d5c4b3a21";
const B = "1c8d5b4f-2e3a-4d6c-8b9f-8a7e6d5c4b32";

describe("service list URL state", () => {
  test("a plain URL is the default state, and the default state a plain URL", () => {
    assert.deepEqual(parseServiceListQuery({}), DEFAULT_SERVICE_LIST);
    assert.deepEqual(serviceListUrl(DEFAULT_SERVICE_LIST), {});
  });
  test("every filter survives a round trip through the URL", () => {
    const s = { ...DEFAULT_SERVICE_LIST, q: "shop", criticality: [A, "none"], owner: B, role: "technical" as const, mine: true, ownerState: "disabled" as const, includeInactive: false, sort: "-memberCount", page: 3, limit: 25 };
    const url = serviceListUrl(s);
    assert.deepEqual(url, { q: "shop", criticality: `${A},none`, owner: B, role: "technical", mine: "1", ownerState: "disabled", inactive: "0", sort: "-memberCount", page: "3", limit: "25" });
    assert.deepEqual(parseServiceListQuery(url), s);
  });
  test("a hand-edited link falls back to the defaults instead of reaching the API", () => {
    const s = parseServiceListQuery({ criticality: "x,,none", owner: "nope", role: "boss", mine: "yes", ownerState: "gone", sort: "id", page: "-2", limit: "9999" });
    assert.deepEqual(s, { ...DEFAULT_SERVICE_LIST, criticality: ["none"], limit: 200 });
  });
  test("the request: paging as offset, the role only with an owner or My services", () => {
    assert.deepEqual(serviceListParams({ ...DEFAULT_SERVICE_LIST, page: 2, role: "business" }), { limit: 50, offset: 50, sort: "criticality" });
    assert.deepEqual(serviceListParams({ ...DEFAULT_SERVICE_LIST, mine: true, role: "business", includeInactive: false, criticality: [A, B] }), {
      limit: 50,
      offset: 0,
      sort: "criticality",
      mine: "true",
      ownerRole: "business",
      includeInactive: "false",
      criticalityValueId: `${A},${B}`,
    });
  });
  test("filters, not the sort or the page, decide between 'no match' and 'none yet'", () => {
    assert.equal(hasServiceFilters({ ...DEFAULT_SERVICE_LIST, sort: "name", page: 4 }), false);
    assert.equal(hasServiceFilters({ ...DEFAULT_SERVICE_LIST, includeInactive: false }), true);
    assert.equal(hasServiceFilters({ ...DEFAULT_SERVICE_LIST, q: "  " }), false);
  });
  test("owner cells show two names, then how many more", () => {
    const o = (n: number) => ({ kind: "user" as const, id: String(n), displayName: `U${n}`, active: true });
    assert.deepEqual(ownerCell([o(1), o(2), o(3), o(4)]), { shown: [o(1), o(2)], more: 2 });
    assert.deepEqual(ownerCell([o(1)]), { shown: [o(1)], more: 0 });
  });
});
