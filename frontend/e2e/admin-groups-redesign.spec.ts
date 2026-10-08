import { E2E_USER } from "./global-setup";
import { apiGet, apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Groups in the reference-mockup look (design document §0, step 12f-2): the inventory's head
// band (breadcrumb, title with the count, Create group, intro, search) above the table card, mono teal names and
// numbered pages; the group page has the CI page's head band, its members table the same pills and pages.
test.describe.configure({ mode: "serial" });

const NAME = `E2E look ${Date.now().toString(36)}`;
let groupId = "";

test.beforeAll(async ({ request }) => {
  const g = await apiSend<{ id: string; version: number }>(request, "POST", "/admin/groups", { name: NAME, description: null });
  const users = await apiGet<{ data: { id: string; username: string }[] }>(request, `/admin/users?q=${encodeURIComponent(E2E_USER.username)}`);
  const me = users.data.find((u) => u.username === E2E_USER.username)!;
  await apiSend(request, "PUT", `/admin/groups/${g.id}/members`, { version: g.version, userIds: [me.id] });
  groupId = g.id;
});

test("groups list: head band, mono names, numbered pages", async ({ page, request }, testInfo) => {
  const total = (await apiGet<{ page: { total: number } }>(request, "/admin/groups?limit=1")).page.total;
  await page.goto("/admin/groups");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Groups");
  await expect(head.getByRole("heading", { level: 1, name: "Groups" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(`${total.toLocaleString("en-US")} total`);
  await expect(head.getByRole("link", { name: "Create group" })).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Search")).toBeVisible();

  const list = page.getByRole("region", { name: "Groups" });
  await expect(list.locator("table.list-table a.list-name").first()).toBeVisible();
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-groups-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-groups-dark");
  await chooseTheme(page, "");
});

test("group page: record head band with the tile, member count and pill members", async ({ page }, testInfo) => {
  await page.goto(`/admin/groups/${groupId}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(NAME);
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.getByRole("heading", { level: 1, name: NAME })).toBeVisible();
  const meta = page.getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText("1 member");
  await expect(meta.locator(".record-meta-line")).toContainText("Updated");
  await expect(head.getByRole("button", { name: "More actions" })).toBeVisible();

  const members = page.getByRole("region", { name: "Members" });
  const row = members.getByRole("row").filter({ has: page.getByRole("link", { name: E2E_USER.username, exact: true }) });
  await expect(row.getByRole("link", { name: E2E_USER.username, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(members.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-group-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-group-dark");
  await chooseTheme(page, "");
});

test("groups: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/groups");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1, name: "Gruppen" })).toBeVisible();
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await page.goto(`/admin/groups/${groupId}`);
  await expect(page.getByTestId("record-meta").locator(".badge").first()).toHaveText("1 Mitglied");
});
