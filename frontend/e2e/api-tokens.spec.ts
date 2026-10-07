import type { APIRequestContext } from "@playwright/test";
import { E2E_USER } from "./global-setup";
import { apiGet, apiSend, at, expect, snap, test } from "./support";

// Administration › API tokens, in order: validation, create (secret shown once, copied), list and filters, use, revoke.
test.describe.configure({ mode: "serial" });
test.use({ permissions: ["clipboard-read", "clipboard-write"] });

const stamp = Date.now().toString(36);
const NAME = `E2E backup script ${stamp}`;
let secret = "";

/** A request context with no session: only the bearer token authenticates it. */
async function bearer(playwright: { request: { newContext: (o: object) => Promise<APIRequestContext> } }, baseURL: string) {
  return playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] }, extraHTTPHeaders: { Authorization: `Bearer ${secret}` } });
}

test("the create dialog shows the API's per-field errors next to the fields", async ({ page }) => {
  await page.goto("/admin/api-tokens");
  await expect(page.getByRole("heading", { level: 1, name: "API tokens" })).toBeVisible();
  await page.getByRole("button", { name: "New API token" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await expect(dialog.getByLabel("Name")).toBeFocused();

  // Nothing filled: the dialog says what is missing without a round trip.
  await dialog.getByRole("button", { name: "Create token" }).click();
  await expect(dialog.locator("#token-name-err")).toContainText("Name the token");
  await expect(dialog.locator("#token-profile-err")).toContainText("Choose the profile");

  // An expiry two years ahead is refused by the API, and the error lands next to the date.
  await dialog.getByLabel("Name").fill(NAME);
  await dialog.getByLabel("Permission profile").selectOption({ label: "Administrator" });
  await dialog.locator("#token-expiry").selectOption("custom");
  const far = new Date(Date.now() + 2 * 365 * 86_400_000).toISOString().slice(0, 10);
  await dialog.getByLabel("Expiry date").fill(far);
  await dialog.getByRole("button", { name: "Create token" }).click();
  await expect(dialog.locator("#token-expiry-date-err")).toBeVisible();
  await expect(dialog.getByLabel("Expiry date")).toHaveAttribute("aria-invalid", "true");
  await expect(dialog.getByRole("alert")).toContainText("Not saved — fix the highlighted fields.");

  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("cell", { name: NAME, exact: true })).toHaveCount(0);
});

test("create a token: the secret is shown once, copied, and never listed", async ({ page }) => {
  await page.goto("/admin/api-tokens");
  await page.getByRole("button", { name: "New API token" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await dialog.getByLabel("Name").fill(NAME);
  await expect(dialog.getByLabel("Owner")).toHaveValue(""); // yourself
  await dialog.getByLabel("Permission profile").selectOption({ label: "Administrator" });
  await dialog.locator("#token-expiry").selectOption({ label: "In 30 days" });
  await dialog.getByRole("button", { name: "Create token" }).click();

  const created = page.getByRole("dialog", { name: `API token “${NAME}” created` });
  await expect(created.getByRole("alert")).toContainText("You won't see it again.");
  const field = created.getByLabel("Secret");
  await expect(field).toBeFocused();
  secret = await field.inputValue();
  expect(secret.length).toBeGreaterThan(20);
  await created.getByRole("button", { name: "Copy" }).click();
  await expect(created.getByRole("status")).toHaveText("Copied to the clipboard.");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(secret);
  await expect(created).toContainText(E2E_USER.username);
  await snap(page, "40-api-token-secret");
  await created.getByRole("button", { name: "Done" }).click();
  await expect(created).toBeHidden();

  // Listed by name and prefix; the secret itself is nowhere on the page any more.
  const row = page.getByRole("row").filter({ has: page.getByRole("cell", { name: NAME, exact: true }) });
  await expect(row).toBeVisible();
  await expect(row.getByText("Active", { exact: true })).toBeVisible();
  await expect(row.getByRole("link", { name: E2E_USER.username, exact: true })).toBeVisible();
  await expect(row.getByRole("link", { name: "Administrator" })).toBeVisible();
  const prefix = (await row.locator("code").innerText()).replace("…", "");
  expect(secret.startsWith(prefix)).toBeTruthy();
  expect(await page.content()).not.toContain(secret);
});

test("the secret authenticates API calls, and the list shows the last use", async ({ page, playwright, baseURL }) => {
  const ctx = await bearer(playwright, baseURL!);
  const res = await ctx.get("/api/v1/configuration-items?limit=1");
  expect(res.status()).toBe(200);
  await ctx.dispose();

  await page.goto(`/admin/api-tokens?q=${encodeURIComponent(stamp)}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("cell", { name: NAME, exact: true }) });
  await expect(row.getByRole("cell").nth(6)).not.toHaveText("Never");
});

test("account writes refuse the token, even an Administrator's, while reading accounts accepts it (GH#119)", async ({ playwright, baseURL, request }) => {
  const ctx = await bearer(playwright, baseURL!);
  const list = await ctx.get(`/api/v1/admin/users?q=${encodeURIComponent(E2E_USER.username)}`);
  expect(list.status()).toBe(200);
  const me = ((await list.json()).data as { id: string; username: string }[]).find((u) => u.username === E2E_USER.username);
  expect(me, "the token owner in the user list").toBeTruthy();
  expect((await ctx.get(`/api/v1/admin/users/${me!.id}`)).status()).toBe(200);

  const minted = `e2e-token-minted-${stamp}`;
  const writes = [
    ctx.post("/api/v1/admin/users", { data: { username: minted, email: `${minted}@example.test`, displayName: "Minted by a token", password: "minted-by-a-token-1", profileIds: [] } }),
    ctx.patch(`/api/v1/admin/users/${me!.id}`, { data: { displayName: "Renamed by a token" } }),
    ctx.put(`/api/v1/admin/users/${me!.id}/password`, { data: { password: "set-by-a-token-123" } }),
    ctx.delete(`/api/v1/admin/users/${me!.id}`),
  ];
  for (const res of await Promise.all(writes)) {
    expect(res.status(), `${res.url()} → ${res.status()}`).toBe(403);
    const { error } = await res.json();
    expect(error.code).toBe("FORBIDDEN");
    expect(error.message).toBe("This endpoint needs a signed-in session; API tokens cannot call it");
  }
  await ctx.dispose();

  // Nothing was written.
  const after = await apiGet<{ data: { username: string }[] }>(request, `/admin/users?q=${encodeURIComponent(minted)}`);
  expect(after.data).toHaveLength(0);
});

test("search and filters live in the URL and survive a reload", async ({ page }) => {
  await page.goto("/admin/api-tokens");
  await page.getByRole("searchbox", { name: "Search", exact: true }).fill(stamp);
  await expect(page).toHaveURL(at("/admin/api-tokens", `?q=${stamp}`));
  await page.getByLabel("Status", { exact: true }).selectOption("active");
  await expect(page).toHaveURL(at("/admin/api-tokens", `?q=${stamp}&status=active`));
  await page.reload();
  await expect(page.getByRole("searchbox", { name: "Search", exact: true })).toHaveValue(stamp);
  await expect(page.getByLabel("Status", { exact: true })).toHaveValue("active");
  await expect(page.getByRole("cell", { name: NAME, exact: true })).toBeVisible();

  await page.getByLabel("Status", { exact: true }).selectOption("revoked");
  await expect(page.getByRole("heading", { name: "No API tokens match these filters" })).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page).toHaveURL(at("/admin/api-tokens"));
});

test("revoke asks first, keeps the token listed as revoked, and the secret stops working", async ({ page, playwright, baseURL }) => {
  await page.goto(`/admin/api-tokens?q=${encodeURIComponent(stamp)}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("cell", { name: NAME, exact: true }) });
  await row.getByRole("button", { name: `Actions for ${NAME}` }).click();
  await page.getByRole("menu", { name: `Actions for ${NAME}` }).getByRole("menuitem", { name: "Revoke…" }).click();
  const confirm = page.getByRole("dialog", { name: `Revoke API token “${NAME}”?` });
  await expect(confirm).toContainText(`owned by ${E2E_USER.username}`);
  await expect(confirm).toContainText("refused from now on");
  await snap(page, "41-api-token-revoke");
  await confirm.getByRole("button", { name: "Revoke token" }).click();
  await expect(confirm).toBeHidden();
  await expect(page.getByRole("status").filter({ hasText: `Revoked API token ${NAME}` })).toBeVisible();
  await expect(row.getByText("Revoked", { exact: true })).toBeVisible();
  await expect(row).toContainText(`by ${E2E_USER.username}`);
  await row.getByRole("button", { name: `Actions for ${NAME}` }).click();
  await expect(page.getByRole("menu", { name: `Actions for ${NAME}` }).getByRole("menuitem", { name: "Revoke…" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Clear filters" }).click();
  await snap(page, "43-api-tokens-list");

  await page.getByLabel("Status", { exact: true }).selectOption("revoked");
  await expect(page).toHaveURL(at("/admin/api-tokens", "?status=revoked"));
  await expect(page.getByRole("cell", { name: NAME, exact: true })).toBeVisible();

  const ctx = await bearer(playwright, baseURL!);
  expect((await ctx.get("/api/v1/configuration-items?limit=1")).status()).toBe(401);
  await ctx.dispose();
});

test("a token for an owner with more rights than yours is refused, and the dialog says why", async ({ browser, request }) => {
  const profileName = `E2E user managers ${stamp}`;
  const username = `e2e-token-mgr-${stamp}`;
  const password = "token-manager-password-1";
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: profileName,
    globalPermissions: ["users.manage"],
    classPermissions: [],
  });
  const me = await apiGet<{ user: { id: string } }>(request, "/auth/me");
  const ownerId = me.user.id;
  await apiSend(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName: `E2E Token Manager ${stamp}`, password, profileIds: [profile.id] });

  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();

  await page.goto("/admin/api-tokens");
  await page.getByRole("button", { name: "New API token" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await dialog.getByLabel("Name").fill(`E2E refused ${stamp}`);
  await dialog.getByLabel("Owner").selectOption(ownerId);
  await dialog.getByLabel("Permission profile").selectOption({ label: profileName });
  await dialog.getByRole("button", { name: "Create token" }).click();
  await expect(dialog.getByRole("alert")).toContainText("you cannot create a token for this owner");
  await expect(dialog.getByRole("alert")).toContainText("holds permissions you do not have");
  await snap(page, "42-api-token-forbidden");
  await context.close();
});

test("a token refused because its owner must use two-factor authentication is badged and filterable (GH#200)", async ({ page, request }) => {
  // A service account's token, created from this password-only session while the account needs no second factor.
  const readers = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E token readers ${stamp}`, globalPermissions: [], classPermissions: [] });
  const mfa = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E token MFA ${stamp}`, globalPermissions: [], classPermissions: [], requireMfa: true });
  const svc = await apiSend<{ id: string; username: string }>(request, "POST", "/admin/users", {
    username: `e2e-token-svc-${stamp}`,
    email: `e2e-token-svc-${stamp}@example.test`,
    displayName: `E2E token service ${stamp}`,
    password: "token-service-password-1",
    profileIds: [readers.id],
  });
  const refusedName = `E2E refused for MFA ${stamp}`;
  const expiresAt = new Date(Date.now() + 7 * 86_400_000).toISOString();
  const minted = await apiSend<{ token: { mfaVerified: boolean; refusedForMfa: boolean } }>(request, "POST", "/admin/api-tokens", { name: refusedName, userId: svc.id, profileId: readers.id, expiresAt });
  expect(minted.token).toMatchObject({ mfaVerified: false, refusedForMfa: false });
  // Now the account must use two-factor authentication: the token is refused.
  await apiSend(request, "PATCH", `/admin/users/${svc.id}`, { profileIds: [readers.id, mfa.id] });

  await page.goto(`/admin/api-tokens?q=${encodeURIComponent(stamp)}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("cell", { name: refusedName, exact: true }) });
  const badge = row.getByText("Refused: owner requires MFA");
  await expect(badge).toBeVisible();
  await expect(badge).toHaveAttribute("title", /created from a session signed in with a second factor/);
  await expect(row.getByText("Active", { exact: true })).toBeVisible();
  await snap(page, "44-api-token-refused-for-mfa");

  // The filter lists only refused tokens, is kept in the URL and survives a reload.
  await page.getByLabel("Refused for two-factor only").check();
  await expect(page).toHaveURL(at("/admin/api-tokens", `?q=${stamp}&refusedForMfa=true`));
  await page.reload();
  await expect(page.getByLabel("Refused for two-factor only")).toBeChecked();
  await expect(page.getByRole("cell", { name: refusedName, exact: true })).toBeVisible();
  await expect(page.getByRole("cell", { name: NAME, exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page).toHaveURL(at("/admin/api-tokens"));

  // A new token for the account from this session would be refused too: the dialog shows the server's reason.
  await page.getByRole("button", { name: "New API token" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await dialog.getByLabel("Name").fill(`E2E not created ${stamp}`);
  await dialog.getByLabel("Owner").selectOption(svc.id);
  await dialog.getByLabel("Permission profile").selectOption({ label: `E2E token readers ${stamp}` });
  await dialog.getByRole("button", { name: "Create token" }).click();
  const alert = dialog.getByRole("alert");
  await expect(alert).toContainText("Not created — the token would be refused.");
  await expect(alert).toContainText(`${svc.username} must use two-factor authentication`);
  await expect(alert.getByRole("link", { name: "Open your account settings" })).toHaveAttribute("href", "/account");
  await snap(page, "45-api-token-mfa-required");
  await dialog.getByRole("button", { name: "Cancel" }).click();
});

test("a user's page links to their API tokens", async ({ page }) => {
  await page.goto(`/admin/users?q=${encodeURIComponent(E2E_USER.username)}`);
  await page.getByRole("link", { name: E2E_USER.username, exact: true }).click();
  await page.getByRole("link", { name: "API tokens of this user" }).click();
  await expect(page).toHaveURL(/\/admin\/api-tokens\?userId=/);
  await expect(page.getByLabel("Owner", { exact: true })).toHaveValue(/.+/);
});
