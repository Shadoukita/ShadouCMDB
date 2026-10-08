import { apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Areas in the reference-mockup look (design document §0, step 12f-9): the inventory's head band
// (breadcrumb, title with the count, New area, intro, show archived) above the areas, mono teal area and class names
// and state pills.
test.describe.configure({ mode: "serial" });

const STAMP = Date.now().toString(36);
const AREA = `E2E look area ${STAMP}`;
const CLASS = `E2E look area class ${STAMP}`;

test.beforeAll(async ({ request }) => {
  const area = await apiSend<{ id: string }>(request, "POST", "/areas", { key: `e2e_look_${STAMP}`, name: AREA, icon: "layers", color: "#0f766e" });
  await apiSend(request, "POST", "/ci-classes", { key: `e2e_look_area_${STAMP}`, name: CLASS, areaId: area.id });
});

test("areas: head band, mono names, state pills", async ({ page }, testInfo) => {
  await page.goto("/admin/areas");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Areas");
  await expect(head.getByRole("heading", { level: 1, name: "Areas" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("button", { name: "New area", exact: true })).toBeVisible();
  await expect(head.locator(".page-intro code").first()).toBeVisible();
  await expect(head.getByLabel(/Show archived areas/)).toBeVisible();

  const list = page.getByRole("region", { name: "Areas" });
  await expect(list.locator("table.reorderable")).toHaveClass(/\blist-table\b/);
  await expect(list.locator("table.list-table th").nth(1)).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row").filter({ hasText: AREA });
  await expect(row.locator("button.list-name")).toContainText(AREA);
  await expect(row.getByRole("link", { name: CLASS, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(row.locator(".badge.ok")).toHaveCSS("border-radius", "999px");

  await checkA11y(page, testInfo, "admin-areas-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-areas-dark");
  await chooseTheme(page, "");
});

test("areas: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/areas");
  const head = page.locator(".list-head");
  await expect(head.getByRole("heading", { level: 1 })).toHaveText("Bereiche");
  await expect(head.locator(".count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(head.getByRole("button", { name: "Neuer Bereich", exact: true })).toBeVisible();
  await expect(head.getByLabel(/Archivierte Bereiche anzeigen/)).toBeVisible();
  const row = page.getByRole("row").filter({ hasText: AREA });
  await expect(row.locator(".badge.ok")).toHaveText("Aktiv");
});
