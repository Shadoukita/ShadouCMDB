import { E2E_USER } from "./global-setup";
import { checkA11y, chooseTheme, csrf, expect, test } from "./support";

// Administration › API tokens in the reference-mockup look (design document §0, step 12f-4): the inventory's
// head band (breadcrumb, title with the count, New API token, intro, search and filters) above the table card,
// mono teal owners, status pills with a dot and numbered pages; the token dialog keeps the dialog title band
// after the secret is shown. Creates one token and revokes it at the end.
test.describe.configure({ mode: "serial" });

const NAME = `E2E redesign ${Date.now().toString(36)}`;
let tokenId = "";

test.afterAll(async ({ request }) => {
  if (tokenId) await request.delete(`/api/v1/admin/api-tokens/${tokenId}`, { headers: { "X-CSRF-Token": await csrf(request) } });
});

test("token dialog: the created state keeps the dialog's title band", async ({ page }) => {
  await page.goto("/admin/api-tokens");
  await page.locator(".list-head").getByRole("button", { name: "New API token" }).click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await dialog.getByLabel("Name").fill(NAME);
  await dialog.getByLabel("Permission profile").selectOption({ label: "Administrator" });
  const response = page.waitForResponse((r) => r.url().endsWith("/api/v1/admin/api-tokens") && r.request().method() === "POST");
  await dialog.getByRole("button", { name: "Create token" }).click();
  tokenId = ((await (await response).json()) as { token: { id: string } }).token.id;

  const created = page.getByRole("dialog", { name: `API token “${NAME}” created` });
  const title = created.locator(":scope > h2");
  await expect(title).toHaveCSS("border-bottom-width", "1px");
  await expect(created.locator("dl.props dd.mono").first()).toHaveText(E2E_USER.username);
  await created.getByRole("button", { name: "Done" }).click();
  await expect(created).toBeHidden();
});

test("tokens list: head band, mono owners, status pills, numbered pages", async ({ page }, testInfo) => {
  await page.goto("/admin/api-tokens");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("API tokens");
  await expect(head.getByRole("heading", { level: 1, name: "API tokens" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("button", { name: "New API token" })).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Search")).toBeVisible();
  await expect(head.getByRole("search").getByLabel("Status", { exact: true })).toBeVisible();

  const list = page.getByRole("region", { name: "API tokens" });
  const row = list.getByRole("row").filter({ has: page.getByRole("cell", { name: NAME, exact: true }) });
  await expect(row.getByRole("link", { name: E2E_USER.username, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-tokens-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-tokens-dark");
  await chooseTheme(page, "");
});

test("tokens: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/api-tokens");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1, name: "API-Tokens" })).toBeVisible();
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(page.getByRole("columnheader", { name: "Besitzer" })).toBeVisible();
});
