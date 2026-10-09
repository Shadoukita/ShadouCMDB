import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// The operator's own account in the reference-mockup look (design document §1.9 U1 and U2, step 10-5): the CI page
// head band with who is signed in, the password form as one grid with its button in the panel's footer, and the
// recovery codes with a title at the panel title's size and a styled copy status. The specs share the e2e admin's
// session, so nothing is changed for real: account-password.spec.ts and mfa.spec.ts walk the real changes, and the
// recovery codes are answered here.

const CODES = ["ab12-cd34", "ef56-gh78", "ij90-kl12", "mn34-op56", "qr78-st90", "uv12-wx34", "yz56-ab78", "cd90-ef12", "gh34-ij56", "kl78-mn90"];

/** Theme, density and locale, and two-factor authentication answered as on, so the recovery codes can be renewed. */
async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  await page.route("**/api/v1/auth/mfa", (route) =>
    route.fulfill({ json: { totpEnabled: true, required: false, enrolmentRequired: false, recoveryCodesRemaining: 7 } }),
  );
  await page.route("**/api/v1/auth/mfa/recovery-codes", (route) => route.fulfill({ json: { codes: CODES } }));
}

async function showCodes(page: Page, labels = { newCodes: "New recovery codes", password: "Current password", code: "Authentication code", submit: "Create new recovery codes" }) {
  await page.getByRole("button", { name: labels.newCodes }).click();
  await page.getByLabel(labels.password).last().fill("not-checked-here");
  await page.getByLabel(labels.code).fill("123456");
  await page.getByRole("button", { name: labels.submit }).click();
  await expect(page.getByRole("list", { name: /Recovery codes|Wiederherstellungscodes/ })).toBeVisible();
}

test("account: the head band, one password grid and its footer", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto("/account");
  const head = page.locator(".record-head-plain");
  await expect(head.getByRole("heading", { level: 1, name: "My account" })).toBeVisible();
  await expect(head.locator(".class-tile")).toBeVisible();
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".mono").first()).toHaveText("e2e-admin");
  expect(await meta.locator(".mono").first().evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  await expect(meta).toContainText("Local account");

  // U1: the three fields share one grid, and the button sits in the panel's footer.
  const panel = page.getByRole("region", { name: "Password", exact: true });
  await expect(panel.locator(".form-grid")).toHaveCount(1);
  await expect(panel.locator(".form-grid input[type=password]")).toHaveCount(3);
  const footer = panel.locator(".form-footer");
  await expect(footer.getByRole("button", { name: "Change password" })).toBeVisible();
  expect(await footer.evaluate((el) => getComputedStyle(el).borderTopWidth)).toBe("1px");
  const current = (await page.locator("#own-current-password").boundingBox())!;
  const fresh = (await page.locator("#own-new-password").boundingBox())!;
  expect(Math.round(current.y), "current and new password on one row at 1440 px").toBe(Math.round(fresh.y));
  await expect(page.locator("main [style]")).toHaveCount(0);

  await snap(page, "account-en-light");
  await checkA11y(page, testInfo, "account-light");

  // The API's per-field errors still land next to the fields.
  await footer.getByRole("button", { name: "Change password" }).click();
  await expect(page.locator("#own-current-password-err")).toHaveText("Required");
  await checkA11y(page, testInfo, "account-error");
});

test("account: the recovery codes' title and copy status (U2)", async ({ page, context }, testInfo) => {
  await prepare(page);
  await page.goto("/account");
  await showCodes(page);
  const title = page.getByRole("heading", { level: 3, name: "Your recovery codes" });
  await expect(title).toHaveCSS("font-size", "15px");
  await expect(title).toHaveCSS("font-weight", "600");

  // The copy failed: the status takes the field-error colour, not the unstyled .error.
  await context.grantPermissions([]);
  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText: () => Promise.reject(new Error("denied")) } });
    document.execCommand = () => false;
  });
  await page.getByRole("button", { name: "Copy", exact: true }).click();
  const status = page.getByTestId("recovery-copy-status");
  await expect(status).toHaveText("Could not copy — select the codes and copy them by hand.");
  await expect(status).toHaveClass(/failed/);
  await expect(status).not.toHaveClass(/(^|\s)error(\s|$)/);
  const danger = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--c-danger-text").trim());
  expect(danger).not.toBe("");
  expect(await status.evaluate((el) => getComputedStyle(el).color)).not.toBe(await page.locator("main").evaluate((el) => getComputedStyle(el).color));

  await snap(page, "account-codes-en-light");
  await checkA11y(page, testInfo, "account-codes");
});

test("account: the dark theme", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto("/account");
  await expect(page.getByRole("heading", { level: 1, name: "My account" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "account-dark");
  await showCodes(page);
  await checkA11y(page, testInfo, "account-codes-dark");
});

test("account: the texts come from the German catalog and fit at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.goto("/account");
  await expect(page.getByRole("heading", { level: 1, name: "Mein Konto" })).toBeVisible();
  await expect(page.getByTestId("record-meta")).toContainText("Lokales Konto");
  await expect(page.locator(".form-footer").getByRole("button", { name: "Passwort ändern" })).toBeVisible();
  expect(await page.locator("main").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`account screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ page }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          await page.setViewportSize({ width, height: 900 });
          await prepare(page, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
          });
          await page.goto("/account");
          await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
          await expect(page.locator(".badges")).toBeVisible();
          await snap(page, `account-${locale}-${theme}-${density}-${width}`);
          await showCodes(
            page,
            locale === "de"
              ? { newCodes: "Neue Wiederherstellungscodes", password: "Aktuelles Passwort", code: "Authentifizierungscode", submit: "Neue Wiederherstellungscodes erstellen" }
              : undefined,
          );
          await snap(page, `account-codes-${locale}-${theme}-${density}-${width}`);
        });
