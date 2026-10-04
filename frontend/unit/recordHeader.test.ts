// Unit tests for the record header's title rule (design §2.2): hostname-like names are set in mono. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { isHostLike } from "../src/lib/format";

describe("isHostLike", () => {
  test("hostnames and FQDNs are", () => {
    for (const n of ["fra1-esx-01", "db01.example.com", "SRV42", "core-sw.fra1.corp"]) assert.equal(isHostLike(n), true, n);
  });
  test("plain words, phrases and other text are not", () => {
    for (const n of ["Oracle", "Platform Engineering", "CRM database (prod)", "-leading", "trailing.", "", "a..b"]) assert.equal(isHostLike(n), false, n);
  });
});
