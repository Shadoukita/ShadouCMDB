import { E2E_USER } from "./global-setup";
import { apiGet, apiSend, csrf, expect, test } from "./support";

// Administration › Users in the new look (SHAA-2530, after #759): the actions the redesign moved. The row menu
// (Open, API tokens, Changes by this user), the record head band's links and its `⋯` menu with Delete user,
// which the API then confirms. A user of its own, so the walk leaves nothing behind.

type UserRow = { id: string; username: string };

const stamp = Date.now().toString(36);
const USERNAME = `e2e-actions-${stamp}`;

test.describe.configure({ mode: "serial" });
let userId = "";

test.beforeAll(async ({ request }) => {
  const body = { username: USERNAME, displayName: `E2E Actions ${stamp}`, email: `${USERNAME}@example.test`, password: "a long enough password", profileIds: [] };
  userId = (await apiSend<UserRow>(request, "POST", "/admin/users", body)).id;
});

// The last test deletes the user; if an earlier one failed first, delete it here.
test.afterAll(async ({ request }) => {
  if ((await request.get(`/api/v1/admin/users/${userId}`)).ok())
    await request.delete(`/api/v1/admin/users/${userId}`, { headers: { "X-CSRF-Token": await csrf(request) } });
});

test("users list: the row menu opens the user, their API tokens and their changes", async ({ page }) => {
  for (const [item, path, search] of [
    ["Open", `/admin/users/${userId}`, ""],
    ["API tokens of this user", "/admin/api-tokens", `?userId=${userId}`],
    ["Changes by this user", "/admin/audit", `?actorId=${userId}`],
  ] as const) {
    await page.goto(`/admin/users?q=${USERNAME}`);
    const row = page.getByRole("region", { name: "Users" }).getByRole("row").filter({ has: page.getByRole("link", { name: USERNAME, exact: true }) });
    await row.getByRole("button", { name: `Actions for ${USERNAME}` }).click();
    const menu = page.getByRole("menu", { name: `Actions for ${USERNAME}` });
    await expect(menu.getByRole("menuitem")).toHaveText(["Open", "API tokens of this user", "Changes by this user"]);
    await menu.getByRole("menuitem", { name: item }).click();
    await expect(page).toHaveURL((url) => url.pathname === path && url.search === search);
  }
});

test("users list: ↓ and ↑ on a row link move between rows", async ({ page }) => {
  await page.goto("/admin/users");
  const links = page.getByRole("region", { name: "Users" }).locator("tbody tr a.list-name");
  test.skip((await links.count()) < 2, "needs two users on the first page");
  await links.nth(0).focus();
  await page.keyboard.press("ArrowDown");
  await expect(links.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(links.nth(0)).toBeFocused();
});

test("user page: the head band links to the user's API tokens and changes", async ({ page }) => {
  await page.goto(`/admin/users/${userId}`);
  const head = page.locator(".record-head");
  await head.getByRole("link", { name: "API tokens of this user" }).click();
  await expect(page).toHaveURL((url) => url.pathname === "/admin/api-tokens" && url.search === `?userId=${userId}`);
  await page.goto(`/admin/users/${userId}`);
  await head.getByRole("link", { name: "Changes by this user" }).click();
  await expect(page).toHaveURL((url) => url.pathname === "/admin/audit" && url.search === `?actorId=${userId}`);
});

test("your own user page has no ⋯ menu: you cannot delete yourself here", async ({ page, request }) => {
  const me = (await apiGet<{ data: UserRow[] }>(request, `/admin/users?q=${encodeURIComponent(E2E_USER.username)}`)).data.find(
    (u) => u.username === E2E_USER.username,
  )!;
  await page.goto(`/admin/users/${me.id}`);
  await expect(page.locator(".record-head").getByRole("heading", { level: 1, name: me.username })).toBeVisible();
  await expect(page.getByRole("button", { name: "More actions" })).toHaveCount(0);
});

test("user page: ⋯ › Delete user asks, deletes, returns to the list, and the API agrees", async ({ page, request }) => {
  await page.goto(`/admin/users/${userId}`);
  await page.locator(".record-head").getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menu", { name: "More actions" }).getByRole("menuitem", { name: "Delete user" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete user ${USERNAME}?` });
  await expect(dialog).toBeVisible();

  // Cancel keeps the user.
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  expect((await request.get(`/api/v1/admin/users/${userId}`)).status()).toBe(200);

  await page.locator(".record-head").getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menu", { name: "More actions" }).getByRole("menuitem", { name: "Delete user" }).click();
  await dialog.getByRole("button", { name: "Delete user" }).click();
  await expect(page).toHaveURL((url) => url.pathname === "/admin/users");
  await expect(page.getByRole("status").filter({ hasText: `Deleted user ${USERNAME}.` })).toBeVisible();
  expect((await request.get(`/api/v1/admin/users/${userId}`)).status()).toBe(404);
  await page.goto(`/admin/users?q=${USERNAME}`);
  await expect(page.getByRole("link", { name: USERNAME, exact: true })).toHaveCount(0);
});
