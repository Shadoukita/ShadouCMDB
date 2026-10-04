import { checkA11y, classIdByName, createCi, csrf, expect, test } from "./support";

// The inventory's facet panel (SHAA-1670 rollout 5f): counts per class, criticality and lookup
// value from GET /configuration-items/facets, each counted without its own filter. Ticking a value
// writes the list's URL filters, like the toolbar and the query bar.

// Deleted afterwards: "e2e-facet-…" sorts before the demo servers, which later specs open by label.
const created: string[] = [];

test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of created.splice(0)) {
    const res = await request.delete(`/api/v1/configuration-items/${id}`, { headers });
    expect(res.ok(), `delete ${id} → ${res.status()}`).toBeTruthy();
  }
});

test("inventory: the facet panel counts the filtered CIs and ticks filters into the URL", async ({ page, request }, testInfo) => {
  const stamp = `e2e-facet-${Date.now().toString(36)}`;
  const serverId = await classIdByName(request, "Server");
  const appId = await classIdByName(request, "Application");
  for (const n of ["a", "b"]) created.push((await createCi(request, serverId, `${stamp}-srv-${n}`)).id);
  created.push((await createCi(request, appId, `${stamp}-app`)).id);

  await page.goto(`/cis?q=${stamp}`);
  await expect(page.locator("table.data tbody tr")).toHaveCount(3);
  const panel = page.getByRole("region", { name: "Facets", exact: true });
  const classes = panel.getByRole("group", { name: "Filter by Class" });
  const server = classes.getByRole("checkbox", { name: /^Server 2$/ });
  const app = classes.getByRole("checkbox", { name: /^Application 1$/ });
  await expect(server).not.toBeChecked();
  await expect(app).toBeVisible();
  // Lookup lists are facets too: the status every created CI holds.
  await expect(panel.getByRole("group", { name: "Filter by Status" }).getByRole("checkbox", { name: /^In service 3$/ })).toBeVisible();

  // Ticking a class filters the list; the other class keeps its count (a facet ignores its own filter).
  await server.check();
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}(&|$)`));
  await expect(page.locator("table.data tbody tr")).toHaveCount(2);
  await expect(server).toBeChecked();
  await expect(app).toBeVisible();
  await expect(panel.getByRole("group", { name: "Filter by Status" }).getByRole("checkbox", { name: /^In service 2$/ })).toBeVisible();

  // A second class widens the list; the class select shows the set, the query bar both keys.
  await app.check();
  await expect(page).toHaveURL(/classId=[^&]*%2C|classId=[^&]*,/);
  await expect(page.locator("table.data tbody tr")).toHaveCount(3);
  await expect(page.locator("#f-class option:checked")).toHaveText("Several values");
  await server.uncheck();
  await expect(page).toHaveURL(new RegExp(`classId=${appId}(&|$)`));
  await expect(page.locator("table.data tbody tr")).toHaveCount(1);

  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await checkA11y(page, testInfo, `facets-${colorScheme}`, { include: ".facets", strict: true });
  }

  // A collapsed group and a hidden panel are remembered in this browser.
  await panel.getByRole("button", { name: "Class", exact: true }).click();
  await expect(classes).toHaveCount(0);
  await page.reload();
  await expect(panel.getByRole("button", { name: "Class", exact: true })).toHaveAttribute("aria-expanded", "false");
  await page.getByRole("button", { name: "Hide facets" }).click();
  await expect(panel).toHaveCount(0);
  await page.reload();
  await expect(page.locator("table.data tbody tr")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Show facets" })).toHaveAttribute("aria-expanded", "false");
  await expect(panel).toHaveCount(0);
  await page.getByRole("button", { name: "Show facets" }).click();
  await panel.getByRole("button", { name: "Class", exact: true }).click();
  await expect(panel.getByRole("group", { name: "Filter by Class" })).toBeVisible();
});

test("inventory: the facet panel starts closed on a narrow screen", async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 900 });
  await page.goto("/cis");
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Facets", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Show facets" }).click();
  await expect(page.getByRole("region", { name: "Facets", exact: true }).getByRole("group", { name: "Filter by Class" })).toBeVisible();
});
