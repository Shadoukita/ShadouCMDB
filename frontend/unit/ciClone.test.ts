// Unit tests for what a CI clone copies (SHAA-2358, gap G11) and the QR label (G12). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { cloneClearedKeys, clonedValue } from "../src/lib/ciClone";
import { qrFileName, qrLabelSvg, qrMatrix } from "../src/lib/qr";

const defs = [
  { id: "a-name", key: "hostname", dataType: "text", systemRole: null },
  { id: "a-ip", key: "ip_address", dataType: "ip", systemRole: null },
  { id: "a-cpu", key: "cpu_cores", dataType: "integer", systemRole: null },
  { id: "a-mail", key: "email", dataType: "text", systemRole: "person_email" },
  { id: "a-state", key: "state", dataType: "lookup", systemRole: null },
];

describe("cloneClearedKeys", () => {
  test("the title attribute, system-role fields and IP addresses are left empty", () => {
    assert.deepEqual(cloneClearedKeys(defs, "a-name"), ["hostname", "ip_address", "email"]);
  });
  test("a reference to a CI the user may not view is left empty", () => {
    assert.deepEqual(cloneClearedKeys(defs, null, { cpu_cores: { hidden: false }, state: { hidden: true } }), ["ip_address", "email", "state"]);
  });
  test("a class without a title attribute clears only the others", () => {
    assert.deepEqual(cloneClearedKeys(defs, null), ["ip_address", "email"]);
  });
});

describe("clonedValue", () => {
  const src = {
    attributes: { hostname: "srv-01", cpu_cores: 16, state: "in-use", ip_address: "10.0.0.1" },
    cleared: new Set(["hostname", "ip_address"]),
    reset: new Set(["state"]),
  };
  test("copies the source's value", () => {
    assert.equal(clonedValue(src, { key: "cpu_cores", defaultValue: 4 }), 16);
  });
  test("a cleared field starts empty, not at its default", () => {
    assert.equal(clonedValue(src, { key: "hostname", defaultValue: "new" }), undefined);
  });
  test("a field a workflow drives starts at its default", () => {
    assert.equal(clonedValue(src, { key: "state", defaultValue: "planned" }), "planned");
  });
  test("a field the source has no value for stays empty", () => {
    assert.equal(clonedValue(src, { key: "rack", defaultValue: "R1" }), undefined);
  });
});

describe("QR label", () => {
  test("the code has a quiet zone and grows with the value", () => {
    const short = qrMatrix("https://cmdb.example/cis/1");
    assert.equal(short.extent, short.count + 8);
    assert.ok(qrMatrix(`https://cmdb.example/cis/${"x".repeat(200)}`).count > short.count);
  });
  test("the SVG is standalone and escapes the caption", () => {
    const svg = qrLabelSvg({ value: "https://cmdb.example/cis/1", title: "db <prod> & co", subtitle: "CI-1" });
    assert.ok(svg.startsWith("<?xml"));
    assert.ok(svg.includes('xmlns="http://www.w3.org/2000/svg"'));
    assert.ok(svg.includes("db &#60;prod&#62; &#38; co"));
    assert.ok(!svg.includes("<prod>"));
    assert.ok(svg.includes(">CI-1</text>"));
  });
  test("file names keep only safe characters", () => {
    assert.equal(qrFileName("CI-7K3M9Q2X", "png"), "CI-7K3M9Q2X-qr.png");
    assert.equal(qrFileName("a/b\\c d", "svg"), "a_b_c_d-qr.svg");
  });
});
