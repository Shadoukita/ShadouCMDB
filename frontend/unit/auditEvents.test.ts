// Unit tests for how audit entries read in an event stream (SHAA-1670 rollout 8, audit A7). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { actionLabel, actionTone, AUDIT_ACTIONS, formatUtc } from "../src/lib/auditEvents";

afterEach(() => setLocaleForTests(null));

describe("audit actions", () => {
  test("every action the API records has a readable name in en and de", () => {
    for (const locale of ["en", "de"] as const) {
      setLocaleForTests(locale);
      const labels = AUDIT_ACTIONS.map(actionLabel);
      for (const [i, a] of AUDIT_ACTIONS.entries()) assert.notEqual(labels[i], a, `${locale}: ${a} has no name`);
      assert.equal(new Set(labels).size, labels.length, `${locale}: two actions share a name`);
    }
  });
  test("an action newer than this client shows raw", () => {
    assert.equal(actionLabel("something.new"), "something.new");
  });
  test("tones: added, removed, refused and workflow steps differ from the neutral rest", () => {
    assert.equal(actionTone("create"), "ok");
    assert.equal(actionTone("login.success"), "ok");
    assert.equal(actionTone("delete"), "danger");
    assert.equal(actionTone("login.failure"), "danger");
    assert.equal(actionTone("mfa.failure"), "danger");
    assert.equal(actionTone("login.locked"), "warn");
    assert.equal(actionTone("schema_change.refused"), "warn");
    assert.equal(actionTone("workflow.start"), "info");
    assert.equal(actionTone("update"), "");
    assert.equal(actionTone("token.use"), "");
  });
  test("times are UTC to the second", () => {
    assert.equal(formatUtc("2026-10-07T10:12:13.266Z"), "2026-10-07 10:12:13");
  });
});
