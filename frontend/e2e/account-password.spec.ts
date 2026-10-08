import type { Browser, Page } from "@playwright/test";
import { apiSend, at, expect, snap, test } from "./support";

// Self-service password change (GH#128): a user without users.manage changes their own password under My account.
// The API keeps this session and ends the others; the old password stops working. An identity provider's account
// has no password here, so the panel points to the provider instead.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const USERNAME = `e2e-pw-${stamp}`;
const OLD_PASSWORD = `pw-e2e-old-${stamp}`;
const NEW_PASSWORD = `pw-e2e-new-${stamp}`;

async function signIn(browser: Browser, password: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(USERNAME);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  // Sign-in hashes with argon2id; give a busy host longer than the default 5 s.
  await page.waitForURL((url) => url.pathname !== "/login", { timeout: 20_000 });
  return page;
}

test.beforeAll(async ({ request }) => {
  await apiSend(request, "POST", "/admin/users", { username: USERNAME, email: `${USERNAME}@example.test`, displayName: `Password operator ${stamp}`, password: OLD_PASSWORD, profileIds: [] });
});

test("an operator changes their own password: checks next to the fields, other sessions end, the old password stops working", async ({ browser }) => {
  test.slow(); // four sign-ins, each an argon2id check
  const other = await signIn(browser, OLD_PASSWORD);
  await expect(other).toHaveURL(at("/"));
  const page = await signIn(browser, OLD_PASSWORD);
  await expect(page).toHaveURL(at("/"));

  await page.goto("/account");
  const panel = page.getByRole("region", { name: "Password", exact: true });
  await expect(panel.getByText("signs you out on every other browser and device")).toBeVisible();

  // Checked in the browser first: missing, too short, not repeated.
  await panel.getByRole("button", { name: "Change password" }).click();
  await expect(page.locator("#own-current-password-err")).toHaveText("Required");
  await expect(page.locator("#own-new-password-err")).toHaveText("Too short");
  await expect(panel.getByLabel("Current password")).toBeFocused();
  await panel.getByLabel("Current password").fill(OLD_PASSWORD);
  await page.locator("#own-new-password").fill(NEW_PASSWORD);
  await panel.getByLabel("Repeat new password").fill(`${NEW_PASSWORD}x`);
  await panel.getByRole("button", { name: "Change password" }).click();
  await expect(page.locator("#own-confirm-password-err")).toHaveText("The passwords do not match");

  // A wrong current password is the API's answer, shown next to its field.
  await panel.getByLabel("Current password").fill("not-the-password");
  await panel.getByLabel("Repeat new password").fill(NEW_PASSWORD);
  await panel.getByRole("button", { name: "Change password" }).click();
  await expect(page.locator("#own-current-password-err")).toContainText("current password is wrong");
  await expect(panel.getByLabel("Current password")).toHaveValue("");
  await snap(page, "account-password-wrong-current");

  await panel.getByLabel("Current password").fill(OLD_PASSWORD);
  await panel.getByRole("button", { name: "Change password" }).click();
  await expect(panel.getByRole("status")).toHaveText("Password changed. Your other sessions were ended.");
  await expect(page.locator("#own-new-password")).toHaveValue("");
  await snap(page, "account-password-changed");

  // This session stays; the other one was ended.
  await page.reload();
  await expect(page.getByRole("heading", { level: 1, name: "My account" })).toBeVisible();
  await other.goto("/account");
  await expect(other).toHaveURL(/\/login/);

  // The old password no longer signs in; the new one does.
  await other.getByLabel("Username").fill(USERNAME);
  await other.getByLabel("Password").fill(OLD_PASSWORD);
  await other.getByRole("button", { name: "Sign in" }).click();
  await expect(other.getByRole("alert")).toContainText("Wrong username or password", { timeout: 20_000 });
  await other.getByLabel("Password").fill(NEW_PASSWORD);
  await other.getByRole("button", { name: "Sign in" }).click();
  await expect(other).toHaveURL(at("/account"), { timeout: 20_000 });

  await other.context().close();
  await page.context().close();
});

test("an identity provider's account has no password form, only a pointer to the provider", async ({ browser }) => {
  const page = await signIn(browser, NEW_PASSWORD);
  await expect(page).toHaveURL(at("/"));
  // Accounts are only created by a real sign-in through a provider; show this one as one.
  await page.route("**/api/v1/auth/me", async (route) => {
    const res = await route.fetch();
    const session = await res.json();
    session.user.identityProvider = { id: "00000000-0000-4000-8000-000000000001", name: "Contoso Entra ID", kind: "oidc" };
    await route.fulfill({ response: res, json: session });
  });
  await page.goto("/account");
  const panel = page.getByRole("region", { name: "Password", exact: true });
  await expect(panel.getByTestId("own-provider-credentials")).toContainText("You sign in through Contoso Entra ID");
  await expect(panel.getByRole("button", { name: "Change password" })).toHaveCount(0);
  await expect(page.locator("#own-current-password")).toHaveCount(0);
  // As in support.ts (GH#774): a refetch still in the route handler fails the test once the context closes.
  await page.unrouteAll({ behavior: "ignoreErrors" });
  await page.context().close();
});
