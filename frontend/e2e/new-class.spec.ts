import { apiGet, apiSend, classIdByName, pickCi, snap, expect, test } from "./support";

// A class created through the API alone gets a sidebar entry, dashboard row, form and
// detail view with no frontend change: the form is generated from its attribute definitions.
test("a new CI class works end to end without a frontend change", async ({ page, request }) => {
  const stamp = Date.now();
  const className = `Load balancer ${stamp}`;
  const serverId = await classIdByName(request, "Server");
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `load_balancer_${stamp}`, name: className });
  const attr = (body: Record<string, unknown>) => apiSend(request, "POST", "/attribute-definitions", { classId: cls.id, ...body });
  await attr({ key: "vip", label: "Virtual IP", dataType: "ip", isRequired: true, groupName: "Traffic", sortOrder: 10 });
  await attr({ key: "algorithm", label: "Algorithm", dataType: "enum", enumValues: ["round_robin", "least_conn"], isRequired: true, groupName: "Traffic", sortOrder: 20 });
  await attr({ key: "max_connections", label: "Max connections", dataType: "integer", validation: { min: 1, max: 100000 }, groupName: "Traffic", sortOrder: 30 });
  await attr({ key: "vip_network", label: "VIP network", dataType: "cidr", groupName: "Traffic", sortOrder: 40 });
  await attr({ key: "ssl_offload", label: "SSL offload", dataType: "boolean", groupName: "Security", sortOrder: 50 });
  await attr({ key: "primary_backend", label: "Primary backend", dataType: "reference", referenceClassId: serverId, groupName: "Backends", sortOrder: 60 });

  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: new RegExp(`^${className}`) })).toBeVisible();
  await expect(page.getByRole("link", { name: `New ${className}` })).toBeVisible();

  await page.goto(`/cis/new?classId=${cls.id}`);
  await expect(page.locator("fieldset.group legend")).toHaveText(["Traffic", "Security", "Backends"]);
  await expect(page.locator("#attr-max_connections-hint")).toHaveText("1 – 100000");
  await expect(page.locator("#attr-vip_network-hint")).toContainText("CIDR");

  // The class has no name or status attribute and no title attribute: only its own fields are asked for.
  await expect(page.locator("#attr-name")).toHaveCount(0);
  await page.getByRole("button", { name: `Create ${className}` }).click();
  await expect(page.locator("#attr-vip-err")).toHaveText("Required");
  await expect(page.locator("#attr-algorithm-err")).toHaveText("Required");

  await page.locator("#attr-vip").fill("not-an-ip");
  await page.locator("#attr-algorithm").selectOption("least_conn");
  await page.locator("#attr-max_connections").fill("0");
  await page.getByRole("button", { name: `Create ${className}` }).click();
  await expect(page.locator("#attr-vip-err")).toBeVisible();
  await expect(page.locator("#attr-max_connections-err")).toBeVisible();
  await snap(page, "13-new-class-form-errors");

  await page.locator("#attr-vip").fill("10.30.0.10");
  await page.locator("#attr-max_connections").fill("5000");
  await page.locator("#attr-vip_network").fill("10.30.0.0/24");
  await page.locator("#attr-ssl_offload").selectOption("true");
  await pickCi(page, "#attr-primary_backend", "fra1-esx", "fra1-esx-01");
  await page.getByRole("button", { name: `Create ${className}` }).click();

  // Without a title attribute a CI is labelled by its generated ident.
  await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
  const created = await apiGet<{ ident: string; label: string }>(request, `/configuration-items/${page.url().split("/").pop()}`);
  expect(created.label).toBe(created.ident);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(created.ident);
  const attrs = page.locator("dl.props").nth(1);
  await expect(attrs).toContainText("10.30.0.10");
  await expect(attrs).toContainText("least_conn");
  await expect(attrs).toContainText("5000");
  await expect(attrs).toContainText("10.30.0.0/24");
  await expect(attrs).toContainText("Yes");
  await snap(page, "14-new-class-detail");
  await attrs.getByRole("link", { name: "fra1-esx-01" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("fra1-esx-01");
});
