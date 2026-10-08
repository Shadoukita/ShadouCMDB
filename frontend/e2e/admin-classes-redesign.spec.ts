import { apiGet, apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › CI classes in the reference-mockup look (design document §0, step 12f-6): the inventory's head
// band (breadcrumb, title with the count, New class, intro, archived and area filters) above the class tree, mono
// teal names and state pills; the class page has the CI page's head band, its attribute tables the same pills.
test.describe.configure({ mode: "serial" });

const STAMP = Date.now().toString(36);
const NAME = `E2E look ${STAMP}`;
let classId = "";

test.beforeAll(async ({ request }) => {
  const areas = await apiGet<{ data: { id: string; isActive: boolean }[] }>(request, "/areas");
  const area = areas.data.find((a) => a.isActive)!;
  const c = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `e2e_look_${STAMP}`, name: NAME, areaId: area.id, color: "#0f766e", icon: "server" });
  classId = c.id;
  await apiSend(request, "POST", "/attribute-definitions", { classId, key: "rack_unit", label: "Rack unit", dataType: "text" });
});

test("classes list: head band, mono names, state pills", async ({ page }, testInfo) => {
  await page.goto("/admin/classes");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("CI classes");
  await expect(head.getByRole("heading", { level: 1, name: "CI classes" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("link", { name: "New class" })).toBeVisible();
  await expect(head.getByLabel("Show archived classes")).toBeVisible();
  await expect(head.getByLabel("Area")).toBeVisible();

  const list = page.getByRole("region", { name: "CI classes" });
  await expect(list.locator("table.list-table th").nth(1)).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row").filter({ has: page.getByRole("link", { name: NAME, exact: true }) });
  await expect(row.getByRole("link", { name: NAME, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(row.locator(".badge.ok")).toHaveCSS("border-radius", "999px");

  await checkA11y(page, testInfo, "admin-classes-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-classes-dark");
  await chooseTheme(page, "");
});

test("class page: record head band with the tinted tile, area chip, state and attribute tables", async ({ page }, testInfo) => {
  await page.goto(`/admin/classes/${classId}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(NAME);
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.locator(".class-tile")).toHaveAttribute("style", /--tile-c:\s*#0f766e/);
  await expect(head.getByRole("heading", { level: 1, name: NAME })).toBeVisible();
  const meta = page.getByTestId("record-meta");
  await expect(meta.locator("a.record-class-chip")).toHaveAttribute("href", /\/admin\/classes\?areaId=/);
  await expect(meta.locator(".badge.ok").locator(".status-dot")).toHaveCount(1);
  await expect(meta.locator(".record-meta-line")).toContainText("Updated");
  await expect(head.getByRole("link", { name: "Open inventory" })).toBeVisible();
  await expect(head.getByRole("button", { name: "More actions" })).toBeVisible();
  await expect(page.locator("table.attributes")).toHaveClass(/\blist-table\b/);
  await expect(page.locator("table.attributes th").nth(1)).toHaveCSS("text-transform", "none");

  await checkA11y(page, testInfo, "admin-class-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-class-dark");
  await chooseTheme(page, "");
});

test("classes: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/classes");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1 })).toHaveText("CI-Klassen");
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await page.goto(`/admin/classes/${classId}`);
  await expect(page.getByTestId("record-meta").locator(".badge.ok")).toHaveText("Aktiv");
});
