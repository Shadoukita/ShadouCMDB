import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// Forced two-factor enrolment in the reference-mockup look (design document §2.7 "Sign-in and first run", step
// 10-4, U3): the sign-in card at 640 px instead of a page header and panels on the grey page, the reason as the
// lead, the set-up steps in the card, and who is signed in with the way out in the foot. The shared e2e session
// has no profile that requires two-factor, so the session and the set-up are answered here and nothing is
// changed on the server; mfa.spec.ts walks the real forced enrolment.

const SECRET = "JBSWY3DPEHPK3PXPJBSWY3DP";
const CODES = ["a1b2-c3d4", "e5f6-g7h8", "j9k0-m1n2", "p3q4-r5s6", "t7u8-v9w0", "x1y2-z3a4", "b5c6-d7e8", "f9g0-h1j2", "k3m4-n5p6", "q7r8-s9t0"];

/** Theme, density and locale, then a session that must set up two-factor (or sign in again with a code). */
async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de"; alreadySetUp?: boolean } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  const mfa = { totpEnabled: !!opts.alreadySetUp, required: true, enrolmentRequired: true, recoveryCodesRemaining: opts.alreadySetUp ? 10 : 0 };
  await page.route("**/api/v1/auth/me", async (route) => {
    const res = await route.fetch();
    const session = await res.json();
    await route.fulfill({ response: res, json: { ...session, mfa: { ...session.mfa, ...mfa } } });
  });
  await page.route("**/api/v1/auth/mfa", (route) => route.fulfill({ json: mfa }));
}

test("two-factor set-up: the sign-in card at 640 px, the reason as the lead, identity and sign-out in the foot", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto("/two-factor-setup");
  const card = page.getByTestId("two-factor-setup");
  await expect(card.getByRole("heading", { level: 1, name: "Set up two-factor authentication" })).toBeVisible();
  await expect(card.locator(".bare-brand .brand-name")).toBeVisible();
  await expect(card.locator(".lead")).toHaveText(/^A permission profile you hold requires two-factor authentication\./);
  // No page header, no panel: the card is the page.
  await expect(page.locator(".page-header, .panel")).toHaveCount(0);

  const viewport = page.viewportSize()!;
  const box = (await card.boundingBox())!;
  expect(Math.round(box.width)).toBe(640);
  expect(Math.abs(box.x + box.width / 2 - viewport.width / 2), "card centred horizontally").toBeLessThanOrEqual(1);
  expect(await card.evaluate((el) => getComputedStyle(el).borderTopLeftRadius)).toBe("12px");

  // The password is focused, and the primary button spans the card like the field.
  const password = card.getByLabel("Current password");
  await expect(password).toBeFocused();
  const start = card.getByRole("button", { name: "Set up authenticator app" });
  expect((await start.boundingBox())!.width).toBeCloseTo((await password.boundingBox())!.width, 0);

  const identity = card.locator(".bare-foot").getByTestId("two-factor-setup-identity");
  await expect(identity).toHaveText(/^Signed in as .+ \(e2e-admin\)\.$/);
  expect(await identity.locator("code").evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  await expect(card.locator(".bare-foot").getByRole("button", { name: "Set it up later? Sign out" })).toHaveClass(/btn-link/);
  await expect(page.locator(".bare [style]")).toHaveCount(0);
  await snap(page, "two-factor-setup-en-light");
  await checkA11y(page, testInfo, "two-factor-setup-light");

  // A missing password is told next to the field.
  await start.click();
  await expect(page.locator("#mfa-currentPassword-err")).toBeVisible();
  await expect(password).toHaveAttribute("aria-invalid", "true");

  // The scan step stays in the card: QR code, setup key in mono, the code from the app.
  await page.route("**/api/v1/auth/mfa/totp", (route) =>
    route.fulfill({ json: { secret: SECRET, otpauthUri: `otpauth://totp/ShadouCMDB:e2e-admin?secret=${SECRET}&issuer=ShadouCMDB`, algorithm: "SHA1", digits: 6, period: 30 } }),
  );
  await password.fill("any-password");
  await start.click();
  await expect(card.getByRole("img", { name: "QR code to add ShadouCMDB to your authenticator app" })).toBeVisible();
  await expect(card.getByLabel("Setup key")).toHaveValue("JBSW Y3DP EHPK 3PXP JBSW Y3DP");
  expect(await card.getByLabel("Setup key").evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  await expect(card.getByLabel("Code from the app")).toBeFocused();
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await snap(page, "two-factor-setup-scan-en-light");
  await checkA11y(page, testInfo, "two-factor-setup-scan");

  // Recovery codes, still in the card.
  await page.route("**/api/v1/auth/mfa/totp/confirm", (route) => route.fulfill({ json: { codes: CODES } }));
  await card.getByLabel("Code from the app").fill("123456");
  await card.getByRole("button", { name: "Verify and turn on" }).click();
  await expect(card.getByRole("list", { name: "Recovery codes" }).getByRole("listitem")).toHaveCount(10);
  await snap(page, "two-factor-setup-codes-en-light");
  await checkA11y(page, testInfo, "two-factor-setup-codes");
});

test("two-factor set-up: the way out signs out and returns to the sign-in card", async ({ page }) => {
  await prepare(page);
  await page.goto("/two-factor-setup?redirect=/cis");
  await expect(page.getByRole("heading", { level: 1, name: "Set up two-factor authentication" })).toBeVisible();
  // The session is not signed out for real: the other specs share it.
  await page.route("**/api/v1/auth/logout", (route) => route.fulfill({ status: 204 }));
  await page.unroute("**/api/v1/auth/me");
  await page.route("**/api/v1/auth/me", (route) =>
    route.fulfill({ status: 401, json: { error: { code: "UNAUTHENTICATED", message: "Sign in to use this endpoint" } } }),
  );
  await page.getByRole("button", { name: "Set it up later? Sign out" }).click();
  await expect(page).toHaveURL((url) => url.pathname === "/login" && url.search === "?redirect=/cis");
  await expect(page.getByRole("heading", { level: 1, name: "Sign in" })).toBeVisible();
});

test("two-factor set-up: already set up elsewhere, the card asks for a sign-in with a code", async ({ page }, testInfo) => {
  await prepare(page, { alreadySetUp: true });
  await page.goto("/two-factor-setup");
  const card = page.getByTestId("two-factor-setup");
  await expect(card.getByRole("heading", { level: 1, name: "Sign in again with a code" })).toBeVisible();
  await expect(card.locator(".lead")).toHaveText(/^Your authenticator app is already set up/);
  await expect(card.getByRole("button", { name: "Sign out and sign in with a code" })).toHaveClass(/block/);
  // One way out only: the primary button.
  await expect(card.locator(".bare-foot").getByRole("button")).toHaveCount(0);
  await snap(page, "two-factor-setup-again-en-light");
  await checkA11y(page, testInfo, "two-factor-setup-again");
});

test("two-factor set-up: the dark theme puts the card on the dark canvas", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto("/two-factor-setup");
  await expect(page.getByRole("heading", { level: 1, name: "Set up two-factor authentication" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".bare")).toHaveCSS("background-color", "rgb(14, 17, 22)");
  await checkA11y(page, testInfo, "two-factor-setup-dark");
});

test("two-factor set-up: the texts come from the German catalog and fit the card at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.goto("/two-factor-setup");
  const card = page.getByTestId("two-factor-setup");
  await expect(card.getByRole("heading", { level: 1, name: "Zwei-Faktor-Authentifizierung einrichten" })).toBeVisible();
  await expect(card.getByTestId("two-factor-setup-identity")).toHaveText(/^Angemeldet als .+ \(e2e-admin\)\.$/);
  await expect(card.getByRole("button", { name: "Später einrichten? Abmelden" })).toBeVisible();
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});
