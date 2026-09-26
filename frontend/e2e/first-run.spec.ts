import { expect, snap, test } from "./support";

// The real first-run setup, with nothing mocked, against a second API whose database has been migrated
// but has no user yet (E2E_FRESH_BASE_URL; CI starts one). The shared instance the other specs use
// already has an administrator, so this cannot run there. Running it consumes the instance: afterwards
// it has a user, so point E2E_FRESH_BASE_URL at a newly migrated database for every run.
const freshURL = process.env.E2E_FRESH_BASE_URL;
test.skip(!freshURL, "E2E_FRESH_BASE_URL (an API on an empty, migrated database) is not set");
test.use({ baseURL: freshURL, storageState: { cookies: [], origins: [] } });

const ADMIN = { username: "first-admin", displayName: "First Administrator", password: "first-run-password-123" };

test("first-run setup creates the administrator once, signs them in, and then closes", async ({ page, playwright }) => {
  const anon = await playwright.request.newContext({ baseURL: freshURL });
  expect((await (await anon.get("/api/v1/setup")).json()).setupRequired).toBe(true);
  // An empty database still protects its data: no session, no access.
  expect((await anon.get("/api/v1/configuration-items")).status()).toBe(401);

  // Every page, sign-in included, leads to setup while no user exists.
  await page.goto("/cis");
  await expect(page).toHaveURL(/\/setup$/);
  await page.goto("/login");
  await expect(page).toHaveURL(/\/setup$/);
  await expect(page.getByRole("heading", { name: /create the first administrator/ })).toBeVisible();

  // The server's own validation answers, not just the form's.
  await page.locator("#setup-username").fill(ADMIN.username);
  await page.locator("#setup-displayName").fill(ADMIN.displayName);
  await page.locator("#setup-password").fill("short");
  await page.locator("#setup-confirm").fill("short");
  await page.getByRole("button", { name: "Create administrator and sign in" }).click();
  await expect(page.locator("#setup-password")).toHaveAttribute("aria-invalid", "true");
  expect((await (await anon.get("/api/v1/setup")).json()).setupRequired).toBe(true);

  await page.locator("#setup-password").fill(ADMIN.password);
  await page.locator("#setup-confirm").fill(ADMIN.password);
  await snap(page, "40-first-run-filled");
  await page.getByRole("button", { name: "Create administrator and sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await expect(page.getByRole("banner").getByText(ADMIN.displayName)).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Administration", exact: true })).toBeVisible();

  // The new session is a real one, holding the built-in Administrator profile.
  const me = await (await page.request.get("/api/v1/auth/me")).json();
  expect(me.user.username).toBe(ADMIN.username);
  expect(me.permissions.administrator).toBe(true);
  expect(me.user.profiles.map((p: { name: string }) => p.name)).toEqual(["Administrator"]);

  // Setup is now closed, to the UI and to the API alike.
  expect((await (await anon.get("/api/v1/setup")).json()).setupRequired).toBe(false);
  const again = await anon.post("/api/v1/setup", { data: { username: "second-admin", displayName: "Second", password: "another-password-123" } });
  expect(again.status()).toBe(409);
  expect((await again.json()).error.code).toBe("CONFLICT");

  // Sign out, then back in with the credentials chosen during setup.
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login$/);
  await page.goto("/setup");
  await expect(page).not.toHaveURL(/\/setup$/);
  await page.goto("/login");
  await page.getByLabel("Username").fill(ADMIN.username);
  await page.getByLabel("Password").fill(ADMIN.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();

  // Only one user exists: the second setup attempt created nothing.
  const users = await (await page.request.get("/api/v1/admin/users")).json();
  expect(users.data.map((u: { username: string }) => u.username)).toEqual([ADMIN.username]);
  await anon.dispose();
});
