// Unit tests for a business service's Members tab URL state, the member picker's errors and the Impact tab's
// pinned services (SHAA-934). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { ImpactItem } from "../src/lib/impact";
import {
  affectedServices,
  DEFAULT_MEMBERS_STATE,
  memberListQuery,
  membersUrlQuery,
  parseMembersQuery,
  pickerErrors,
} from "../src/lib/serviceMembers";

const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";

describe("Members tab URL state", () => {
  test("a plain link is the default state", () => {
    assert.deepEqual(parseMembersQuery({}), DEFAULT_MEMBERS_STATE);
    assert.deepEqual(membersUrlQuery({ tab: "members" }, DEFAULT_MEMBERS_STATE), { tab: "members" });
  });
  test("round-trips every key and keeps the page's own", () => {
    const query = { tab: "members", mq: "web", mclass: `${A},${B}`, mkind: "service", msort: "-addedAt", mpage: "3" };
    const s = parseMembersQuery(query);
    assert.deepEqual(s, { q: "web", classIds: [A, B], kind: "service", sort: "-addedAt", page: 3 });
    assert.deepEqual(membersUrlQuery({ tab: "members", direction: "upstream", mq: "old" }, s), { ...query, direction: "upstream" });
  });
  test("unusable values fall back to their defaults", () => {
    const s = parseMembersQuery({ mclass: `nope,${A},${A}`, mkind: "server", msort: "id", mpage: "-2" });
    assert.deepEqual(s, { ...DEFAULT_MEMBERS_STATE, classIds: [A] });
    assert.equal(parseMembersQuery({ mpage: "1.5" }).page, 1);
  });
  test("the list query pages at 50 on the server", () => {
    assert.deepEqual(memberListQuery({ ...DEFAULT_MEMBERS_STATE, kind: "ci", page: 2 }), { kind: "ci", sort: "name", limit: 50, offset: 50 });
  });
});

describe("member picker errors", () => {
  const names: Record<string, string> = { [A]: "web-01", [B]: "Shop" };
  test("per-index errors land on the submitted CI, member_limit on the request", () => {
    const { byId, general } = pickerErrors(
      [
        { field: "memberIds", code: "member_limit", message: "A business service can have at most 5000 members" },
        { field: "memberIds[0]", code: "not_found", message: "Configuration item does not exist" },
        { field: "memberIds[1]", code: "membership_cycle", message: "loop" },
      ],
      [A, B],
      (id) => names[id],
      { maxNesting: 10, maxMembers: 5000 },
    );
    assert.equal(byId.get(A)?.message, "Configuration item does not exist.");
    assert.equal(byId.get(B)?.message, "Shop already includes this service. Adding it would create a loop.");
    assert.deepEqual(general, [{ code: "member_limit", message: "A service can have at most 5000 members." }]);
  });
  test("nesting depth names the limit; an unknown code keeps the API's message", () => {
    const { byId } = pickerErrors(
      [
        { field: "memberIds[0]", code: "membership_nesting_depth", message: "x" },
        { field: "memberIds[1]", code: "something_new", message: "Server says no" },
      ],
      [A, B],
      (id) => names[id],
      { maxNesting: 10, maxMembers: 5000 },
    );
    assert.equal(byId.get(A)?.message, "Services can be nested at most 10 levels deep.");
    assert.equal(byId.get(B)?.message, "Server says no");
  });
});

describe("Impact tab: affected business services", () => {
  const item = (id: string, classId: string, hops: number, rank: number | null): ImpactItem =>
    ({ id, name: id, classId, hops, criticality: rank === null ? null : { rank } }) as unknown as ImpactItem;
  test("only the service class, by criticality rank (not set last), then hops, then name", () => {
    const items = [item("z", "svc", 1, null), item("b", "svc", 2, 1), item("a", "svc", 3, 1), item("c", "svc", 1, 2), item("srv", "server", 1, 1)];
    assert.deepEqual(
      affectedServices(items, "svc").map((i) => i.id),
      ["b", "a", "c", "z"],
    );
  });
  test("nothing before the service class is known", () => {
    assert.deepEqual(affectedServices([item("a", "svc", 1, 1)], undefined), []);
  });
});
