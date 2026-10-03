// Unit tests for the Users ↔ Person helpers (SHAA-1505). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { emailErrorMessage, looksLikeEmail, parseSignInStatus, signInStatusLabel } from "../src/lib/people";
import { ssoErrorMessage } from "../src/lib/signIn";

describe("parseSignInStatus", () => {
  test("the API's states pass", () => {
    for (const s of ["ready", "email_required", "person_missing"]) assert.equal(parseSignInStatus(s), s);
  });
  test("anything else from a hand-edited URL is dropped", () => {
    for (const s of ["", "READY", "incomplete", "toString", null, undefined, ["ready"]]) assert.equal(parseSignInStatus(s), undefined);
  });
});

describe("emailErrorMessage", () => {
  test("another account's address", () => {
    const m = emailErrorMessage([{ field: "email", code: "unique", message: "Another account already uses this e-mail address" }]);
    assert.equal(m, "Another account already uses this e-mail address.");
  });
  test("another person's address", () => {
    const m = emailErrorMessage([{ field: "email", code: "person_email_taken", message: "Another person already has …" }]);
    assert.match(m ?? "", /^A person who is not this account's already has this e-mail address/);
  });
  test("an unknown code keeps the API's message; other fields are not the e-mail's", () => {
    assert.equal(emailErrorMessage([{ field: "email", code: "too_long", message: "At most 254 characters" }]), "At most 254 characters");
    assert.equal(emailErrorMessage([{ field: "username", code: "unique", message: "Already exists" }]), undefined);
    assert.equal(emailErrorMessage([]), undefined);
  });
  test("follows the locale", () => {
    setLocaleForTests("de");
    try {
      assert.equal(emailErrorMessage([{ field: "email", code: "unique", message: "" }]), "Ein anderes Konto verwendet diese E-Mail-Adresse bereits.");
      assert.equal(signInStatusLabel("person_missing"), "Konto unvollständig");
    } finally {
      setLocaleForTests(null);
    }
  });
});

describe("looksLikeEmail", () => {
  test("a plausible address passes; the API has the last word", () => {
    assert.ok(looksLikeEmail("ada@example.test"));
    assert.ok(!looksLikeEmail("ada"));
    assert.ok(!looksLikeEmail("ada @example.test"));
    assert.ok(!looksLikeEmail(`${"a".repeat(250)}@x.de`));
  });
});

test("an incomplete account's SSO refusal has its own message", () => {
  assert.equal(signInStatusLabel("person_missing"), "Account incomplete");
  assert.match(ssoErrorMessage("account_incomplete") ?? "", /^Your ShadouCMDB account is incomplete/);
});
