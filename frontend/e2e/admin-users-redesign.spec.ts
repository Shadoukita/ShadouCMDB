import { E2E_USER } from "./global-setup";
import { apiGet, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Users in the reference-mockup look (design document §0, step 12f-1): the inventory's head
// band (breadcrumb, title with the count, New user, intro, search and filters) above the table card, mono
// teal usernames, status pills and numbered pages; the user page has the CI page's head band. Read-only.

type UserRow = { id: string; username: string; displayName: string };

async function self(request: Parameters<typeof apiGet>[0]): Promise<UserRow> {
  const list = await apiGet<{ data: UserRow[] }>(request, `/admin/users?q=${encodeURIComponent(E2E_USER.username)}`);
  return list.data.find((u) => u.username === E2E_USER.username)!;
}

test("users list: head band, mono names, status pills, numbered pages", async ({ page, request }, testInfo) => {
  const total = (await apiGet<{ page: { total: number } }>(request, "/admin/users?limit=1")).page.total;
  await page.goto("/admin/users");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Users");
  await expect(head.getByRole("heading", { level: 1, name: "Users" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(`${total.toLocaleString("en-US")} total`);
  await expect(head.getByRole("link", { name: "New user" })).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Search")).toBeVisible();

  const list = page.getByRole("region", { name: "Users" });
  const row = list.getByRole("row").filter({ has: page.getByRole("link", { name: E2E_USER.username, exact: true }) });
  await expect(row.getByRole("link", { name: E2E_USER.username, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-users-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-users-dark");
  await chooseTheme(page, "");
});

test("user page: record head band with the tile, mono name and chips", async ({ page, request }, testInfo) => {
  const me = await self(request);
  await page.goto(`/admin/users/${me.id}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(me.username);
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.getByRole("heading", { level: 1, name: me.username })).toBeVisible();
  const meta = page.getByTestId("record-meta");
  await expect(meta.locator(".badge.ok").first()).toHaveText("Active");
  await expect(meta.locator(".badge").filter({ hasText: "You" })).toBeVisible();
  await expect(meta.locator(".record-meta-line")).toContainText(me.displayName);
  await expect(head.getByRole("link", { name: "API tokens of this user" })).toBeVisible();

  await checkA11y(page, testInfo, "admin-user-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-user-dark");
  await chooseTheme(page, "");
});

test("users: the head texts come from the German catalog", async ({ page, request }) => {
  const me = await self(request);
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/users");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1, name: "Benutzer" })).toBeVisible();
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await page.goto(`/admin/users/${me.id}`);
  await expect(page.getByTestId("record-meta").locator(".badge.ok").first()).toHaveText("Aktiv");
  await expect(page.getByTestId("record-meta").locator(".record-meta-line")).toContainText(/Letzte Anmeldung|Noch nie angemeldet/);
});
