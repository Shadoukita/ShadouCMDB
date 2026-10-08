import { apiGet, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Permission profiles in the reference-mockup look (design document §0, step 12f-3): the
// inventory's head band (breadcrumb, title with the count, New profile, intro, search and sort) above the table
// card, mono teal names, pill badges and numbered pages; the profile page has the CI page's head band, and the
// permission matrix the list tables' sentence-case headers and pills.
test.describe.configure({ mode: "serial" });

type Profile = { id: string; name: string; isBuiltin: boolean; userCount: number };
let admin: Profile;

test.beforeAll(async ({ request }) => {
  const list = await apiGet<{ data: Profile[] }>(request, "/admin/profiles?limit=200");
  admin = list.data.find((p) => p.isBuiltin)!;
});

test("profiles list: head band, mono names, pill badges, numbered pages", async ({ page, request }, testInfo) => {
  const total = (await apiGet<{ page: { total: number } }>(request, "/admin/profiles?limit=1")).page.total;
  await page.goto("/admin/profiles");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Permission profiles");
  await expect(head.getByRole("heading", { level: 1, name: "Permission profiles" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(`${total.toLocaleString("en-US")} total`);
  await expect(head.getByRole("link", { name: "New profile" })).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Search")).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Sort")).toBeVisible();

  const list = page.getByRole("region", { name: "Permission profiles" });
  const row = list.getByRole("row").filter({ has: page.getByRole("link", { name: admin.name, exact: true }) });
  await expect(row.getByRole("link", { name: admin.name, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge").filter({ hasText: "Built-in" })).toBeVisible();
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-profiles-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-profiles-dark");
  await chooseTheme(page, "");
});

test("profile page: record head band with the tile, chips and the matrix", async ({ page }, testInfo) => {
  await page.goto(`/admin/profiles/${admin.id}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(admin.name);
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.getByRole("heading", { level: 1, name: admin.name })).toBeVisible();
  const meta = page.getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText("Built-in");
  const users = meta.getByRole("link", { name: admin.userCount === 1 ? "1 user" : `${admin.userCount} users` });
  await expect(users).toHaveClass(/\brecord-class-chip\b/);
  await expect(meta.locator(".record-meta-line")).toContainText("Updated");
  await expect(head.getByRole("button", { name: "Clone" })).toBeVisible();

  const matrix = page.locator("table.matrix");
  await expect(matrix).toHaveClass(/\blist-table\b/);
  await expect(matrix.getByRole("columnheader", { name: "View" })).toBeVisible();
  await expect(matrix.getByRole("checkbox", { name: "view on all classes" })).toBeChecked();

  await checkA11y(page, testInfo, "admin-profile-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-profile-dark");
  await chooseTheme(page, "");
});

test("new profile: the head band without meta, the matrix editable", async ({ page }) => {
  await page.goto("/admin/profiles/new");
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toBeVisible();
  await expect(head.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByTestId("record-meta")).toHaveCount(0);
  await expect(page.locator("table.matrix").getByRole("checkbox", { name: "view on all classes" })).toBeEnabled();
});

test("profiles: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/profiles");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1, name: "Berechtigungsprofile" })).toBeVisible();
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await page.goto(`/admin/profiles/${admin.id}`);
  await expect(page.getByTestId("record-meta").locator(".badge").first()).toHaveText("Integriert");
  await expect(page.locator("table.matrix").getByRole("columnheader", { name: "Anzeigen" })).toBeVisible();
});
