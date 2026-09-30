// Unit tests for the audit log's client-address hover text (GH#282). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { clientTitle } from "../src/pages/admin/auditClient";

describe("clientTitle", () => {
  test("a sign-in behind a trusted proxy shows the proxy and the forged hop, labelled unverified", () => {
    const title = clientTitle({
      action: "login.locked",
      entityType: "sessions",
      newValue: {
        attemptedUsername: "admin",
        ipAddress: "10.0.5.20",
        peerIpAddress: "127.0.0.1",
        claimedIpAddress: "198.51.100.9",
        userAgent: "curl/8.5",
        lockedForSeconds: 900,
      },
    });
    assert.equal(
      title,
      ["Locked for 900s", "Peer address: 127.0.0.1", "Claimed address (unverified): 198.51.100.9", "Browser: curl/8.5"].join("\n"),
    );
  });

  test("no claimed line when the forwarded hop matches the verified address", () => {
    const title = clientTitle({ action: "login.success", entityType: "sessions", newValue: { username: "a", ipAddress: "10.0.0.1", userAgent: "x" } });
    assert.equal(title, "Browser: x");
  });

  test("token use and MFA rows show the verified address and the claimed one", () => {
    for (const action of ["token.use", "mfa.failure"] as const) {
      const title = clientTitle({
        action,
        entityType: action === "token.use" ? "api_tokens" : "users",
        newValue: { ipAddress: "10.0.0.2", claimedIpAddress: "203.0.113.7" },
      });
      assert.equal(title, "Address: 10.0.0.2\nClaimed address (unverified): 203.0.113.7", action);
    }
  });

  test("other rows have no client hover text, even if a snapshot carries these keys", () => {
    assert.equal(clientTitle({ action: "update", entityType: "users", newValue: { claimedIpAddress: "203.0.113.7" } }), undefined);
  });
});
