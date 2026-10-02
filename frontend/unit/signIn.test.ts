// Unit tests for the sign-in page's ?ssoError= message and ?redirect= check (GH#442). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { setLocaleForTests, t } from "../src/i18n/index";
import { SSO_ERRORS, safeRedirect, ssoErrorMessage } from "../src/lib/signIn";

describe("ssoErrorMessage", () => {
  test("a known code gets its own message", () => {
    assert.equal(ssoErrorMessage("expired"), t(SSO_ERRORS.expired));
    assert.equal(ssoErrorMessage("cancelled"), "The sign-in was cancelled at the identity provider.");
    assert.equal(ssoErrorMessage("mfa_not_enforced"), t(SSO_ERRORS.mfa_not_enforced));
  });
  test("the messages come from the catalog, so they follow the locale", () => {
    setLocaleForTests("de");
    try {
      assert.equal(ssoErrorMessage("cancelled"), "Die Anmeldung wurde beim Identitätsanbieter abgebrochen.");
      assert.equal(ssoErrorMessage("something_new"), "Single Sign-on fehlgeschlagen (something_new). Versuchen Sie es erneut oder wenden Sie sich an einen Administrator.");
    } finally {
      setLocaleForTests(null);
    }
  });
  test("no code, no message", () => {
    assert.equal(ssoErrorMessage(null), null);
    assert.equal(ssoErrorMessage(""), null);
    assert.equal(ssoErrorMessage(["expired"]), null);
  });
  test("an unknown code that looks like a server code is named", () => {
    assert.equal(ssoErrorMessage("something_new"), "Single sign-on failed (something_new). Try again, or ask an administrator.");
    // Object properties are not codes.
    assert.equal(ssoErrorMessage("__proto__"), "Single sign-on failed (__proto__). Try again, or ask an administrator.");
  });
  test("free text from a crafted link is never shown", () => {
    for (const spoof of [
      "Your account is locked. Call IT on +1-555-0100",
      "call_it_on_5550100",
      "a".repeat(33),
      "Expired",
      "expired ",
      "toString",
    ]) {
      assert.equal(ssoErrorMessage(spoof), "Single sign-on failed. Try again, or ask an administrator.", spoof);
    }
  });
});

describe("safeRedirect", () => {
  test("keeps same-app paths, with their query", () => {
    assert.equal(safeRedirect("/cis/42"), "/cis/42");
    assert.equal(safeRedirect("/inventory?q=Müller rack&page=2"), "/inventory?q=Müller rack&page=2");
  });
  test("anything else goes to the start page", () => {
    for (const value of [
      undefined,
      null,
      ["/cis"],
      "",
      "cis/42",
      "https://evil.example",
      "//evil.example",
      "/\\evil.example",
      "/\\\\evil.example",
      "/cis\\..\\..",
      "/\t/evil.example",
      "/\n/evil.example",
      "/cis\u007f",
      `/${"a".repeat(2048)}`,
    ]) {
      assert.equal(safeRedirect(value), "/", JSON.stringify(value));
    }
  });
});
