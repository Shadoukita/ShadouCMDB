import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// The forced e-mail step in the reference-mockup look (design document §2.7 "Sign-in and first run", step 10-3):
// the sign-in card at 400 px, the reason as the lead, a full-width primary button, and who is signed in with a
// "Not you? Sign out" link in the foot. The API cannot create an account without an e-mail any more, so the session
// is answered as it is for such an account; people.spec.ts walks the validation and the save.

/** Theme, density and locale, then the session of an account without an e-mail. */
async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  await page.route("**/api/v1/auth/me", async (route) => {
    const res = await route.fetch();
    const session = await res.json();
    await route.fulfill({ response: res, json: { ...session, emailRequired: true, user: { ...session.user, email: null } } });
  });
}

test("email entry: the sign-in card at 400 px, the reason as the lead, identity and sign-out in the foot", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto("/enter-email");
  const card = page.getByTestId("email-entry");
  await expect(card.getByRole("heading", { level: 1, name: "Enter your e-mail address" })).toBeVisible();
  await expect(card.locator(".bare-brand .brand-name")).toBeVisible();
  await expect(card.locator(".lead")).toContainText("Your account was created before e-mail addresses were required.");

  const viewport = page.viewportSize()!;
  const box = (await card.boundingBox())!;
  expect(Math.round(box.width)).toBe(400);
  expect(Math.abs(box.x + box.width / 2 - viewport.width / 2), "card centred horizontally").toBeLessThanOrEqual(1);
  expect(await card.evaluate((el) => getComputedStyle(el).borderTopLeftRadius)).toBe("12px");

  // The field is focused, and the primary button spans the card like the field.
  const field = card.getByRole("textbox", { name: "E-mail address" });
  await expect(field).toBeFocused();
  const submit = card.getByRole("button", { name: "Save and continue" });
  expect((await submit.boundingBox())!.width).toBeCloseTo((await field.boundingBox())!.width, 0);

  // Who is signed in sits in the foot: the name in bold, the username in mono, then the way out.
  const identity = card.locator(".bare-foot").getByTestId("email-entry-identity");
  await expect(identity).toHaveText(/^Signed in as .+ \(e2e-admin\)\.$/);
  await expect(identity.locator("strong")).not.toBeEmpty();
  expect(await identity.locator("code").evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  const signOut = card.locator(".bare-foot").getByRole("button", { name: "Not you? Sign out" });
  await expect(signOut).toHaveClass(/btn-link/);
  await expect(page.locator(".bare [style]")).toHaveCount(0);

  await snap(page, "email-entry-en-light");
  await checkA11y(page, testInfo, "email-entry-light");

  // A missing address is told next to the field.
  await submit.click();
  await expect(page.locator("#email-entry-err")).toHaveText("Enter your e-mail address.");
  await expect(field).toHaveAttribute("aria-invalid", "true");
  await checkA11y(page, testInfo, "email-entry-error");

  // The way out signs out and returns to the sign-in card. The session is not signed out for real: the
  // other specs share it, so the logout request is answered here.
  await page.route("**/api/v1/auth/logout", (route) => route.fulfill({ status: 204 }));
  await page.unroute("**/api/v1/auth/me");
  await page.route("**/api/v1/auth/me", (route) =>
    route.fulfill({ status: 401, json: { error: { code: "UNAUTHENTICATED", message: "Sign in to use this endpoint" } } }),
  );
  await signOut.click();
  await expect(page).toHaveURL((url) => url.pathname === "/login");
  await expect(page.getByRole("heading", { level: 1, name: "Sign in" })).toBeVisible();
});

test("email entry: the dark theme puts the card on the dark canvas", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto("/enter-email");
  await expect(page.getByRole("heading", { level: 1, name: "Enter your e-mail address" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".bare")).toHaveCSS("background-color", "rgb(14, 17, 22)");
  await checkA11y(page, testInfo, "email-entry-dark");
});

test("email entry: the texts come from the German catalog and fit the card at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.goto("/enter-email");
  const card = page.getByTestId("email-entry");
  await expect(card.getByRole("heading", { level: 1, name: "E-Mail-Adresse eingeben" })).toBeVisible();
  await expect(card.getByRole("textbox", { name: "E-Mail-Adresse" })).toBeVisible();
  await expect(card.getByRole("button", { name: "Speichern und fortfahren" })).toBeVisible();
  await expect(card.getByTestId("email-entry-identity")).toHaveText(/^Angemeldet als .+ \(e2e-admin\)\.$/);
  await expect(card.getByRole("button", { name: "Nicht Sie? Abmelden" })).toBeVisible();
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});
