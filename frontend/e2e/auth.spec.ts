import { E2E_USER } from "./global-setup";
import { at, expect, snap, test } from "./support";

test.describe("signed out", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test("a protected page goes to sign-in; a wrong password is explained; sign-in returns to the page; sign-out", async ({ page }) => {
    await page.goto("/cis?q=crm");
    await expect(page).toHaveURL(/\/login\?redirect=%2Fcis%3Fq%3Dcrm$|\/login\?redirect=\/cis\?q=crm$/);
    await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
    await expect(page.getByLabel("Username")).toBeFocused();
    // No shell, no data before sign-in.
    await expect(page.getByRole("navigation", { name: "Main" })).toHaveCount(0);

    await page.getByLabel("Username").fill(E2E_USER.username);
    await page.getByLabel("Password").fill("not-the-password");
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.getByRole("alert")).toContainText("Wrong username or password");
    await expect(page.getByLabel("Password")).toHaveValue("");
    await snap(page, "20-sign-in-wrong-password");

    await page.getByLabel("Password").fill(E2E_USER.password);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page).toHaveURL(at("/cis", "?q=crm"));
    await expect(page.locator("#f-q")).toHaveValue("crm");
    await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();

    await page.getByRole("button", { name: "Sign out" }).click();
    await expect(page).toHaveURL(/\/login$/);
    await expect(page.getByText("Your session has ended")).toHaveCount(0);
    await page.goto("/");
    await expect(page).toHaveURL(/\/login$/);
  });
});

test("an expired session goes to sign-in and back to where the operator was", async ({ browser }) => {
  // A context of its own, so clearing its cookies does not touch the shared signed-in state.
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(E2E_USER.username);
  await page.getByLabel("Password").fill(E2E_USER.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();

  await page.goto("/cis?q=fra1");
  await expect(page.getByRole("link", { name: "fra1-esx-01", exact: true })).toBeVisible();
  await context.clearCookies(); // the server-side session is gone from the browser's point of view
  await page.locator("#f-q").fill("crm");
  await expect(page).toHaveURL(/\/login\?redirect=/);
  await expect(page.getByRole("status")).toContainText("Your session has ended");
  await snap(page, "21-session-expired");

  await page.getByLabel("Username").fill(E2E_USER.username);
  await page.getByLabel("Password").fill(E2E_USER.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).toHaveURL(at("/cis", "?q=crm"));
  await expect(page.getByRole("link", { name: "crm-db", exact: true })).toBeVisible();
  await context.close();
});

test("first-run setup is shown while the API says no user exists, and signs the administrator in", async ({ page, request }) => {
  // The shared database already has users, so the "no users" answers are simulated; the session returned is real.
  const me = await (await request.get("/api/v1/auth/me")).json();
  let sent: Record<string, unknown> | undefined;
  await page.route("**/api/v1/auth/me", (route) =>
    route.fulfill({ status: 401, json: { error: { code: "UNAUTHENTICATED", message: "Sign in to use this endpoint" } } }),
  );
  await page.route("**/api/v1/setup", async (route) => {
    if (route.request().method() === "GET") return route.fulfill({ json: { setupRequired: true } });
    sent = route.request().postDataJSON();
    return route.fulfill({ status: 201, json: me });
  });

  await page.goto("/cis");
  await expect(page).toHaveURL(/\/setup$/);
  await expect(page.getByRole("heading", { name: /create the first administrator/ })).toBeVisible();
  await page.goto("/login");
  await expect(page).toHaveURL(/\/setup$/); // sign-in makes no sense before an account exists

  await page.getByRole("button", { name: "Create administrator and sign in" }).click();
  await expect(page.locator("#setup-setupToken")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#setup-username")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#setup-username-err")).toHaveText("Required");
  await page.locator("#setup-setupToken").fill("  token-from-the-log  ");
  await page.locator("#setup-username").fill("first-admin");
  await page.locator("#setup-displayName").fill("First Admin");
  await page.locator("#setup-password").fill("a-long-enough-password");
  await page.locator("#setup-confirm").fill("a-different-password!");
  await page.getByRole("button", { name: "Create administrator and sign in" }).click();
  await expect(page.locator("#setup-confirm-err")).toHaveText("The passwords do not match");
  await snap(page, "22-first-run-setup");

  await page.locator("#setup-confirm").fill("a-long-enough-password");
  await page.getByRole("button", { name: "Create administrator and sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  expect(sent).toEqual({
    username: "first-admin",
    displayName: "First Admin",
    email: null,
    password: "a-long-enough-password",
    setupToken: "token-from-the-log",
  });
});
