import type { APIRequestContext } from "@playwright/test";
import { classIdByName, createCi, csrf, expect, test } from "./support";

// The inventory sorts by an attribute of the class (GH#112): sort=[-]attributes.<key> with classId.
// Text sorts case-insensitively, IP addresses in address order, CIs without a value come last.
// Uses the demo template's Server class (hostname: text, ip_address: ip).
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const name = (n: string) => `e2e-sort-${stamp}-${n}`;
let serverId = "";
const created: { id: string; label: string }[] = [];

async function labels(request: APIRequestContext, sort: string): Promise<string[]> {
  const res = await request.get(`/api/v1/configuration-items?classId=${serverId}&q=${encodeURIComponent(`e2e-sort-${stamp}`)}&sort=${sort}&limit=50`);
  expect(res.status(), `sort=${sort} → ${res.status()} ${await res.text()}`).toBe(200);
  return ((await res.json()).data as { label: string }[]).map((c) => c.label);
}

async function sortError(request: APIRequestContext, query: string): Promise<string> {
  const res = await request.get(`/api/v1/configuration-items?${query}`);
  expect(res.status(), `${query} → ${res.status()}`).toBe(400);
  const { error } = await res.json();
  expect(error.details[0].field).toBe("sort");
  return error.details[0].code;
}

test.beforeAll(async ({ request }) => {
  serverId = await classIdByName(request, "Server");
  // Text order of the addresses (10.99.0.10 < 10.99.0.100 < 10.99.0.9) differs from address order.
  created.push(await createCi(request, serverId, name("a"), { hostname: "c-host", ip_address: "10.99.0.100" }));
  created.push(await createCi(request, serverId, name("b"), { hostname: "A-host", ip_address: "10.99.0.9" }));
  created.push(await createCi(request, serverId, name("c"), { hostname: "b-host", ip_address: "10.99.0.10" }));
  created.push(await createCi(request, serverId, name("d")));
});

// These servers have no relationships and their names sort before the demo servers: left behind, they would
// become "the first server by label" that later specs (layout-edit) expect to have relationships.
test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const ci of created) {
    const res = await request.delete(`/api/v1/configuration-items/${ci.id}`, { headers });
    expect(res.status(), `DELETE ${ci.label} → ${res.status()}`).toBe(204);
  }
});

test("the API sorts by an attribute: IPs in address order, text without regard to case, empty values last", async ({ request }) => {
  expect(await labels(request, "attributes.ip_address")).toEqual([name("b"), name("c"), name("a"), name("d")]);
  expect(await labels(request, "-attributes.ip_address")).toEqual([name("a"), name("c"), name("b"), name("d")]);
  expect(await labels(request, "attributes.hostname")).toEqual([name("b"), name("c"), name("a"), name("d")]);
  expect(await labels(request, "-attributes.hostname")).toEqual([name("a"), name("c"), name("b"), name("d")]);
});

test("an attribute sort without one class, or on an attribute the class lacks, is a 400 on sort", async ({ request }) => {
  expect(await sortError(request, "sort=attributes.hostname")).toBe("class_required");
  expect(await sortError(request, `classId=${serverId}&sort=attributes.no_such_attribute_${stamp}`)).toBe("unknown_attribute");
  expect(await sortError(request, `classId=${serverId}&sort=hostname`)).toBe("invalid_format");
});

test("the inventory lists a class in the attribute order given in the URL", async ({ page }) => {
  await page.goto(`/cis?classId=${serverId}&q=${encodeURIComponent(`e2e-sort-${stamp}`)}&sort=-attributes.ip_address`);
  const rows = page.getByRole("table").getByRole("row").filter({ hasText: `e2e-sort-${stamp}` });
  await expect(rows).toHaveCount(4);
  await expect(rows.getByRole("link").filter({ hasText: `e2e-sort-${stamp}` })).toHaveText([name("a"), name("c"), name("b"), name("d")]);
});
