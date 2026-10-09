import type { Page, Route } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// The API does not answer the first request of the page load (step 10-8): the sign-in card names the failure in
// its heading, quotes the API's message in an error alert, offers one full-width Retry, and says in the foot who
// can fix it, with the request id in mono. Before, a bare app name sat over an inline alert with a small button.

async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
}

const MIGRATE = "The database schema is not migrated (0 of 13 migrations applied). Run `shadoucmdb migrate`, then retry.";
const notMigrated = (route: Route) =>
  route.fulfill({ status: 503, json: { error: { code: "SCHEMA_NOT_MIGRATED", message: MIGRATE, requestId: "req-e2e-boot" } } });
const unavailable = (route: Route) =>
  route.fulfill({
    status: 503,
    json: { error: { code: "DATABASE_UNAVAILABLE", message: "The CMDB database did not answer.", requestId: "req-e2e-db" } },
  });

test("boot error: the card names the failure, quotes the API and offers one Retry", async ({ page }, testInfo) => {
  await prepare(page);
  await page.route("**/api/v1/**", notMigrated);
  await page.goto("/cis");
  const card = page.getByTestId("boot-error");
  await expect(card.getByRole("heading", { level: 1, name: "The database is not migrated yet" })).toBeVisible();
  await expect(page).toHaveTitle(/^Cannot start · /);
  await expect(card.locator(".brand-mark")).toBeVisible();
  await expect(card.locator(".lead")).toContainText("cannot open until the server answers");
  await expect(card.getByRole("alert")).toHaveText(MIGRATE);
  await expect(card.getByRole("alert")).toHaveClass(/alert-error/);
  const retry = card.getByRole("button", { name: "Retry" });
  await expect(retry).toHaveClass(/btn-primary/);
  // Full width: the button spans the card's content box.
  const [btn, alert] = await Promise.all([retry.boundingBox(), card.getByRole("alert").boundingBox()]);
  expect(Math.abs(btn!.width - alert!.width)).toBeLessThan(2);
  const foot = card.locator(".bare-foot");
  await expect(foot).toContainText("cannot use its database");
  const id = foot.getByTestId("boot-request-id");
  await expect(id).toContainText("req-e2e-boot");
  expect(await id.locator("code").evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  await expect(page.locator("main")).toHaveCount(1);
  await expect(page.locator("main [style]")).toHaveCount(0);
  await snap(page, "boot-error-en-light");
  await checkA11y(page, testInfo, "boot-error-light");

  await page.unroute("**/api/v1/**", notMigrated);
  await retry.click();
  await expect(page.getByRole("heading", { name: "Configuration items" })).toBeVisible();
  await expect(page).toHaveURL(/\/cis$/);
});

test("boot error: an unreachable API gets the network hint and no request id", async ({ page }) => {
  await prepare(page);
  await page.route("**/api/v1/**", (route) => route.abort("connectionrefused"));
  await page.goto("/");
  const card = page.getByTestId("boot-error");
  await expect(card.getByRole("heading", { level: 1, name: "API unreachable" })).toBeVisible({ timeout: 15_000 });
  await expect(card.locator(".bare-foot")).toContainText("reverse proxy");
  await expect(card.getByTestId("boot-request-id")).toHaveCount(0);
  expect(await card.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

test("boot error: the dark theme", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.route("**/api/v1/**", unavailable);
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "The CMDB database is unavailable" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "boot-error-dark");
});

test("boot error: the texts come from the German catalog and fit at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.route("**/api/v1/**", unavailable);
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "Die CMDB-Datenbank ist nicht verfügbar" })).toBeVisible();
  await expect(page).toHaveTitle(/^Start nicht möglich · /);
  await expect(page.getByRole("button", { name: "Erneut versuchen" })).toBeVisible();
  await expect(page.getByTestId("boot-request-id")).toContainText("Anfrage-ID");
  expect(await page.locator("main").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`boot error screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ page }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          await page.setViewportSize({ width, height: 900 });
          await prepare(page, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
          });
          await page.route("**/api/v1/**", notMigrated);
          await page.goto("/");
          await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
          await snap(page, `boot-error-${locale}-${theme}-${density}-${width}`);
        });
