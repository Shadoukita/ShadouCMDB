import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// First-run setup in the reference-mockup look (design document §2.7 "Sign-in and first run", step 10-2, audit L2):
// the sign-in card at 640 px, the fields in two sections (server access, administrator account) without spacer
// cells, the setup token in mono, a full-width primary button and the closing hint in the foot. The shared database
// already has users, so "no user exists" is simulated; first-run.spec.ts walks the real setup on an empty database.
test.use({ storageState: { cookies: [], origins: [] } });

/** Theme, density and locale for a signed-out page, then the API answers of an install without users. */
async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  await page.route("**/api/v1/auth/me", (route) =>
    route.fulfill({ status: 401, json: { error: { code: "UNAUTHENTICATED", message: "Sign in to use this endpoint" } } }),
  );
  await page.route("**/api/v1/setup", (route) =>
    route.request().method() === "GET" ? route.fulfill({ json: { setupRequired: true } }) : route.abort(),
  );
}

test("setup: the sign-in card at 640 px, two sections without spacer cells, a full-width button", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto("/setup");
  const card = page.locator("form.bare-card");
  await expect(card.getByRole("heading", { level: 1, name: /create the first administrator/ })).toBeVisible();
  await expect(card.locator(".bare-brand .brand-name")).toBeVisible();
  await expect(card.locator(".lead")).toContainText("No user exists yet.");

  const viewport = page.viewportSize()!;
  const box = (await card.boundingBox())!;
  expect(Math.round(box.width)).toBe(640);
  expect(Math.abs(box.x + box.width / 2 - viewport.width / 2), "card centred horizontally").toBeLessThanOrEqual(1);
  expect(await card.evaluate((el) => getComputedStyle(el).borderTopLeftRadius)).toBe("12px");

  // Two labelled sections; no empty grid cells hold the layout together.
  await expect(card.getByRole("group", { name: "Server access" }).getByLabel("Setup token")).toBeVisible();
  const account = card.getByRole("group", { name: "Administrator account" });
  for (const field of ["username", "displayName", "email", "password", "confirm"]) {
    await expect(account.locator(`#setup-${field}`)).toBeVisible();
  }
  await expect(card.locator(".form-grid > div:empty")).toHaveCount(0);

  // The token is focused and set in mono; the token and email fields span the grid.
  const token = card.getByLabel("Setup token");
  await expect(token).toBeFocused();
  expect(await token.evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  const submit = card.getByRole("button", { name: "Create administrator and sign in" });
  const submitWidth = (await submit.boundingBox())!.width;
  expect((await token.boundingBox())!.width).toBeCloseTo(submitWidth, 0);
  expect((await account.locator("#setup-email").boundingBox())!.width).toBeCloseTo(submitWidth, 0);

  await expect(card.locator(".bare-foot .hint")).toHaveText(/Setup closes as soon as this account exists/);
  await expect(page.locator(".bare [style]")).toHaveCount(0);

  await snap(page, "setup-en-light");
  await checkA11y(page, testInfo, "setup-light");

  // Field errors stay next to their fields inside the sections.
  await submit.click();
  await expect(token).toHaveAttribute("aria-invalid", "true");
  await expect(account.locator("#setup-username-err")).toHaveText("Required");
  await checkA11y(page, testInfo, "setup-errors");
});

test("setup: the dark theme puts the card on the dark canvas", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto("/setup");
  await expect(page.getByRole("heading", { level: 1, name: /create the first administrator/ })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator(".bare")).toHaveCSS("background-color", "rgb(14, 17, 22)");
  await checkA11y(page, testInfo, "setup-dark");
});

test("setup: the texts come from the German catalog and fit the card at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.goto("/setup");
  const card = page.locator("form.bare-card");
  await expect(card.getByRole("heading", { level: 1, name: "Willkommen – legen Sie den ersten Administrator an" })).toBeVisible();
  await expect(card.getByRole("group", { name: "Serverzugriff" }).getByLabel("Einrichtungstoken")).toBeVisible();
  await expect(card.getByRole("group", { name: "Administratorkonto" })).toBeVisible();
  await expect(card.getByRole("button", { name: "Administrator anlegen und anmelden" })).toBeVisible();
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});
