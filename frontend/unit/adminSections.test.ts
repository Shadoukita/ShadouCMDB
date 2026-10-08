// Unit tests for which Administration sections a user may open (SHAA-2553: Groups and Permission profiles,
// #764 and #779). They mirror the API: groups need users.manage; profiles are read with profiles.manage or
// users.manage. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { GlobalPermission } from "../src/api/admin";
import { ADMIN_SECTIONS, sectionAllowed, visibleSections } from "../src/pages/admin/sections";

const holding = (...held: GlobalPermission[]) => ({ can: (p: GlobalPermission) => held.includes(p), isAdministrator: false });
const keys = (...held: GlobalPermission[]) => visibleSections(holding(...held)).map((s) => s.key);
const section = (key: string) => ADMIN_SECTIONS.find((s) => s.key === key)!;

describe("Groups and Permission profiles sections", () => {
  test("users.manage opens both", () => {
    assert.ok(sectionAllowed(section("groups"), holding("users.manage")));
    assert.ok(sectionAllowed(section("profiles"), holding("users.manage")));
  });
  test("profiles.manage opens profiles, not groups", () => {
    assert.ok(sectionAllowed(section("profiles"), holding("profiles.manage")));
    assert.ok(!sectionAllowed(section("groups"), holding("profiles.manage")));
    assert.deepEqual(keys("profiles.manage"), ["profiles"]);
  });
  test("other admin rights open neither", () => {
    for (const p of ["datamodel.manage", "workflows.manage", "customization.manage", "config.export_import", "audit.view"] as GlobalPermission[]) {
      assert.ok(!keys(p).includes("groups"), p);
      assert.ok(!keys(p).includes("profiles"), p);
    }
    assert.deepEqual(keys(), []);
  });
});
