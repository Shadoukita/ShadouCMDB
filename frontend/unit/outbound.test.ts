// Unit tests for Administration › Webhooks, Workflow deliveries and Outbound e-mail (SHAA-2737): who sees which
// section (SHAA-2725 §8), how a reason code is explained, and which deliveries the API retries or discards.
// Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { GlobalPermission } from "../src/api/admin";
import { setLocaleForTests } from "../src/i18n";
import { ENDPOINT_KEY_PATTERN, reasonText } from "../src/lib/outbound";
import { canDiscard, canRetry } from "../src/pages/admin/deliveries/deliveryRules";
import { visibleSections } from "../src/pages/admin/sections";

const keys = (...held: GlobalPermission[]) => visibleSections({ can: (p) => held.includes(p), isAdministrator: false }).map((s) => s.key);

describe("sections", () => {
  test("webhooks.manage alone opens Webhooks and Outbound e-mail, not the deliveries (the API needs workflows.manage)", () => {
    assert.deepEqual(keys("webhooks.manage"), ["webhooks", "mail"]);
  });
  test("workflows.manage alone opens all three; Webhooks shows the endpoints by name only", () => {
    assert.deepEqual(keys("workflows.manage"), ["workflows", "workflow-deliveries", "webhooks", "mail"]);
  });
});

describe("reasonText", () => {
  test("a blocked address names the address it resolved to", () => {
    const text = reasonText("address_blocked:10.0.0.1");
    assert.match(text, /10\.0\.0\.1/);
    assert.match(text, /WEBHOOK_ALLOW_PRIVATE_CIDRS/);
  });
  test("an IPv6 address keeps its colons", () => {
    assert.match(reasonText("address_blocked:::ffff:127.0.0.1"), /::ffff:127\.0\.0\.1/);
  });
  test("an unknown code from a newer server is shown as it is", () => {
    assert.equal(reasonText("something_new"), "something_new");
    assert.equal(reasonText(null), "");
  });
  test("German explains it too", () => {
    setLocaleForTests("de");
    try {
      assert.match(reasonText("host_not_allowed"), /erlaubten Hosts/);
    } finally {
      setLocaleForTests(null);
    }
  });
});

describe("retry and discard", () => {
  test("dead and held e-mail and webhook deliveries can be retried; inbox entries cannot", () => {
    assert.ok(canRetry({ status: "dead", kind: "webhook" }));
    assert.ok(canRetry({ status: "held", kind: "email" }));
    assert.ok(!canRetry({ status: "dead", kind: "inbox" }));
    assert.ok(!canRetry({ status: "pending", kind: "webhook" }));
    assert.ok(!canRetry({ status: "discarded", kind: "webhook" }));
  });
  test("waiting and dead deliveries can be discarded; sent, skipped and discarded ones cannot", () => {
    for (const s of ["pending", "held", "dead"] as const) assert.ok(canDiscard({ status: s }), s);
    for (const s of ["sending", "delivered", "skipped", "discarded"] as const) assert.ok(!canDiscard({ status: s }), s);
  });
});

test("endpoint keys follow the API's rule", () => {
  assert.ok(ENDPOINT_KEY_PATTERN.test("itsm-sync_2"));
  assert.ok(!ENDPOINT_KEY_PATTERN.test("2itsm"));
  assert.ok(!ENDPOINT_KEY_PATTERN.test("ITSM"));
});
