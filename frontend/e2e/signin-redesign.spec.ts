import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// Sign-in in the reference-mockup look (design document §2.7 "Sign-in and first run", step 10-1, audit L1): a centred
// 400 px card on the canvas with the brand tile and app name, a display heading, a full-width primary button and the
// hints in the card's foot. The code step is reached with a stubbed MFA_REQUIRED answer; the real two-factor sign-in
// is walked by mfa.spec.ts.
test.use({ storageState: { cookies: [], origins: [] } });

/** Theme, density and locale for a signed-out page: the same browser settings the user menu writes. */
async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
}

/** Answers the password with MFA_REQUIRED, as the API does for an account with two-factor authentication. */
async function stubMfaRequired(page: Page) {
  await page.route("**/api/v1/auth/login", (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: { code: "MFA_REQUIRED", message: "Enter the code from your authenticator app." } }),
    }),
  );
}

test("sign-in: a centred 400 px card with the brand, a display heading and the hints in the foot", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto("/login");
  const card = page.locator("form.bare-card");
  await expect(card.getByRole("heading", { level: 1, name: "Sign in" })).toBeVisible();
  await expect(card.locator(".bare-brand .brand-name")).toBeVisible();
  await expect(card.locator(".lead")).toHaveText("Sign in with your account to continue.");

  const viewport = page.viewportSize()!;
  const box = (await card.boundingBox())!;
  expect(Math.round(box.width)).toBe(400);
  expect(Math.abs(box.x + box.width / 2 - viewport.width / 2), "card centred horizontally").toBeLessThanOrEqual(1);
  expect(Math.abs(box.y + box.height / 2 - viewport.height / 2), "card centred vertically").toBeLessThanOrEqual(2);
  const style = await card.evaluate((el) => {
    const s = getComputedStyle(el);
    const h1 = getComputedStyle(el.querySelector("h1")!);
    return { radius: s.borderTopLeftRadius, shadow: s.boxShadow !== "none", heading: h1.fontSize };
  });
  expect(style).toEqual({ radius: "12px", shadow: true, heading: "28px" });

  // The primary button spans the form; the hints sit under a rule at 12 px.
  const submit = card.getByRole("button", { name: "Sign in" });
  const input = card.getByLabel("Username");
  expect((await submit.boundingBox())!.width).toBeCloseTo((await input.boundingBox())!.width, 0);
  const foot = card.locator(".bare-foot");
  await expect(foot).toContainText("shadoucmdb create-admin");
  await expect(foot.locator(".hint").last()).toHaveCSS("font-size", "12px");
  await expect(page.locator(".bare [style]")).toHaveCount(0);

  await snap(page, "signin-password-en-light");
  await checkA11y(page, testInfo, "signin-light");
});

test("sign-in: the dark theme puts the card on the dark canvas", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto("/login");
  await expect(page.getByRole("heading", { level: 1, name: "Sign in" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".bare")).toHaveCSS("background-color", "rgb(14, 17, 22)");
  await checkA11y(page, testInfo, "signin-dark");
});

test("sign-in: the code step uses the same card with a mono code input", async ({ page }, testInfo) => {
  await prepare(page);
  await stubMfaRequired(page);
  await page.goto("/login");
  await page.getByLabel("Username").fill("operator");
  await page.getByLabel("Password").fill("not-checked");
  await page.getByRole("button", { name: "Sign in" }).click();

  const card = page.locator("form.bare-card");
  await expect(card.getByRole("heading", { level: 1, name: "Two-factor authentication" })).toBeVisible();
  await expect(card.locator(".lead")).toContainText("Signing in as operator.");
  const code = card.getByLabel("Authentication code");
  await expect(code).toBeFocused();
  expect(await code.evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  const foot = card.locator(".bare-foot");
  await expect(foot.getByRole("button", { name: "Lost your device? Use a recovery code" })).toBeVisible();
  await expect(foot.getByRole("button", { name: "Sign in as someone else" })).toBeVisible();
  await snap(page, "signin-code-en-light");
  await checkA11y(page, testInfo, "signin-code-light");

  await foot.getByRole("button", { name: "Lost your device? Use a recovery code" }).click();
  await expect(card.getByLabel("Recovery code")).toBeFocused();
  await foot.getByRole("button", { name: "Sign in as someone else" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Sign in" })).toBeVisible();
});

test("sign-in: the texts come from the German catalog", async ({ page }) => {
  await prepare(page, { locale: "de" });
  await stubMfaRequired(page);
  await page.goto("/login");
  const card = page.locator("form.bare-card");
  await expect(card.getByRole("heading", { level: 1, name: "Anmelden" })).toBeVisible();
  await expect(card.locator(".lead")).toHaveText("Melden Sie sich mit Ihrem Konto an, um fortzufahren.");
  await card.getByLabel("Benutzername").fill("operator");
  await card.getByLabel("Passwort").fill("not-checked");
  await card.getByRole("button", { name: "Anmelden" }).click();
  await expect(card.getByRole("heading", { level: 1, name: "Zwei-Faktor-Authentifizierung" })).toBeVisible();
  // The long German heading wraps at its hyphens inside the card.
  expect(await card.locator("h1").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});
