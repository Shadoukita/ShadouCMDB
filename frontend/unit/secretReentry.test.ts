// Unit tests for the identity-provider form's secret re-entry rule (GH#238). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  ldapEndpoint,
  reentryHint,
  secretMissing,
  secretReentryField,
  secretRequiredFields,
  syncSecret,
  type StoredAddress,
  type TypedAddress,
} from "../src/pages/admin/identity/secretReentry";

const oidc: StoredAddress = { oidc: { issuerUrl: "https://login.example.com/realms/cmdb", clientSecretSet: true }, ldap: null };
const ldap: StoredAddress = { oidc: null, ldap: { url: "ldaps://dc1.example.com", bindDn: "CN=svc,DC=example,DC=com", bindPasswordSet: true } };
const oidcForm = (issuerUrl: string): TypedAddress => ({ kind: "oidc", issuerUrl, url: "", bindDn: "" });
const ldapForm = (url: string, bindDn = "CN=svc,DC=example,DC=com"): TypedAddress => ({ kind: "ldap", issuerUrl: "", url, bindDn });

describe("secretReentryField", () => {
  test("a new provider (nothing stored) never asks", () => {
    assert.equal(secretReentryField(undefined, oidcForm("https://evil.example.net")), null);
  });

  test("OIDC: a changed issuer URL asks for the client secret; the stored value typed back does not", () => {
    assert.equal(secretReentryField(oidc, oidcForm("https://evil.example.net/realms/cmdb")), "oidc.clientSecret");
    assert.equal(secretReentryField(oidc, oidcForm("https://login.example.com/realms/cmdb")), null);
    assert.equal(secretReentryField(oidc, oidcForm("  https://login.example.com/realms/cmdb ")), null);
    // Like the API: host case, the default port and a trailing slash are not a change; the path is.
    assert.equal(secretReentryField(oidc, oidcForm("https://LOGIN.example.com:443/realms/cmdb/")), null);
    assert.equal(secretReentryField(oidc, oidcForm("https://login.example.com/realms/other")), "oidc.clientSecret");
    assert.equal(secretReentryField(oidc, oidcForm("https://login.example.com:8443/realms/cmdb")), "oidc.clientSecret");
  });

  test("OIDC: without a stored secret (public client) nothing is asked", () => {
    const pub: StoredAddress = { oidc: { issuerUrl: "https://login.example.com", clientSecretSet: false } };
    assert.equal(secretReentryField(pub, oidcForm("https://other.example.com")), null);
  });

  test("LDAP: a changed scheme, host or port asks for the bind password", () => {
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://evil.example.net")), "ldap.bindPassword");
    assert.equal(secretReentryField(ldap, ldapForm("ldap://dc1.example.com")), "ldap.bindPassword");
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://dc1.example.com:1636")), "ldap.bindPassword");
  });

  test("LDAP: the same endpoint written differently does not ask", () => {
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://dc1.example.com")), null);
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://DC1.Example.com:636/")), null);
  });

  test("LDAP: a changed bind DN asks; clearing it (anonymous search) does not", () => {
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://dc1.example.com", "CN=admin,DC=example,DC=com")), "ldap.bindPassword");
    assert.equal(secretReentryField(ldap, ldapForm("ldaps://evil.example.net", "")), null);
  });

  test("LDAP: without a stored password nothing is asked", () => {
    const anon: StoredAddress = { ldap: { url: "ldaps://dc1.example.com", bindDn: null, bindPasswordSet: false } };
    assert.equal(secretReentryField(anon, ldapForm("ldaps://evil.example.net", "CN=svc,DC=x")), null);
  });
});

describe("ldapEndpoint", () => {
  test("fills in the default port and lowercases the host", () => {
    assert.equal(ldapEndpoint("ldap://DC1.example.com"), "ldap://dc1.example.com:389");
    assert.equal(ldapEndpoint("ldaps://dc1.example.com"), "ldaps://dc1.example.com:636");
    assert.equal(ldapEndpoint("ldaps://[::1]:1636"), "ldaps://[::1]:1636");
  });
  test("leaves what it cannot parse as typed (trimmed)", () => {
    assert.equal(ldapEndpoint(" not a url "), "not a url");
  });
});

describe("syncSecret", () => {
  test("required: keep-stored turns into an empty input; typed values and removals stay", () => {
    assert.equal(syncSecret(undefined, true), "");
    assert.equal(syncSecret("", true), "");
    assert.equal(syncSecret("s3cret", true), "s3cret");
    assert.equal(syncSecret(null, true), null);
  });
  test("no longer required: an empty input goes back to keep-stored; typed values stay", () => {
    assert.equal(syncSecret("", false), undefined);
    assert.equal(syncSecret(undefined, false), undefined);
    assert.equal(syncSecret("s3cret", false), "s3cret");
    assert.equal(syncSecret(null, false), null);
  });
});

test("secretMissing: nothing typed is missing, a typed value or a removal is an answer", () => {
  assert.equal(secretMissing(undefined), true);
  assert.equal(secretMissing(""), true);
  assert.equal(secretMissing("x"), false);
  assert.equal(secretMissing(null), false);
});

test("secretRequiredFields: picks the secret fields a 422 names with code secret_required", () => {
  const error = {
    status: 422,
    code: "VALIDATION_ERROR",
    details: [
      { field: "ldap.bindPassword", code: "secret_required", message: "Enter the bind password / client secret again when the server address changes." },
      { field: "ldap.url", code: "invalid", message: "Not a URL" },
      { field: "name", code: "secret_required", message: "not a secret field" },
    ],
  };
  assert.deepEqual(secretRequiredFields(error), ["ldap.bindPassword"]);
  assert.deepEqual(secretRequiredFields({ details: [{ field: "oidc.clientSecret", code: "secret_required" }] }), ["oidc.clientSecret"]);
  assert.deepEqual(secretRequiredFields(new Error("network")), []);
  assert.deepEqual(secretRequiredFields(null), []);
});

test("reentryHint names the secret", () => {
  assert.equal(reentryHint("ldap.bindPassword"), "The server address changed. Enter the bind password again.");
  assert.equal(reentryHint("oidc.clientSecret"), "The server address changed. Enter the client secret again.");
});
