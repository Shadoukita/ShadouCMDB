import type { Page } from "@playwright/test";
import { checkA11y, expect, snap, test } from "./support";

// An address that matches no screen, in the reference-mockup look (design document §1.9 F3, step 10-6): the page
// head band with the title, an "Error 404" badge and the requested address in mono, then a panel with the way back,
// instead of a bare empty state floating on the canvas.

async function prepare(page: Page, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de" } = {}) {
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
}

const ADDRESS = "/no/such/screen?view=42";

test("not found: the head band names the address, the panel offers the way back", async ({ page }, testInfo) => {
  await prepare(page);
  await page.goto(ADDRESS);
  await expect(page).toHaveTitle(/Not found/);
  const head = page.locator(".record-head-plain");
  await expect(head.getByRole("heading", { level: 1, name: "Page not found" })).toBeVisible();
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.getByTestId("record-meta")).toContainText("Error 404");
  const path = head.getByTestId("not-found-path");
  await expect(path).toHaveText(ADDRESS);
  expect(await path.evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");

  const panel = page.locator("main .panel");
  await expect(panel.getByRole("heading", { level: 2, name: "No screen at this address" })).toBeVisible();
  await expect(panel.locator(".state-icon")).toBeVisible();
  await expect(panel.getByRole("link", { name: "Go to the dashboard" })).toHaveClass(/btn-primary/);
  await expect(panel.getByRole("link", { name: "Open the inventory" })).toHaveAttribute("href", "/cis");
  await expect(page.locator("main [style]")).toHaveCount(0);

  await snap(page, "not-found-en-light");
  await checkA11y(page, testInfo, "not-found-light");

  await panel.getByRole("link", { name: "Open the inventory" }).click();
  await expect(page).toHaveURL(/\/cis$/);
  await page.goBack();
  await panel.getByRole("link", { name: "Go to the dashboard" }).click();
  await expect(page).toHaveURL(/\/$/);
});

test("not found: the dark theme", async ({ page }, testInfo) => {
  await prepare(page, { theme: "dark" });
  await page.goto(ADDRESS);
  await expect(page.getByRole("heading", { level: 1, name: "Page not found" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "not-found-dark");
});

test("not found: the texts come from the German catalog and fit at 900 px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await prepare(page, { locale: "de" });
  await page.goto(ADDRESS);
  await expect(page.getByRole("heading", { level: 1, name: "Seite nicht gefunden" })).toBeVisible();
  await expect(page.getByTestId("record-meta")).toContainText("Fehler 404");
  await expect(page.getByRole("link", { name: "Inventar öffnen" })).toBeVisible();
  expect(await page.locator("main").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`not found screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ page }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          await page.setViewportSize({ width, height: 900 });
          await prepare(page, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
          });
          await page.goto(ADDRESS);
          await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
          await snap(page, `not-found-${locale}-${theme}-${density}-${width}`);
        });
