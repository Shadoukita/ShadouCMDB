import { checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Starter templates in the reference-mockup look (design document §0, step 9c-4): the CI page's
// head band without tabs (tile, template count, installed pill), each template a panel with its status pill and the
// install button at the end of the header, and no style attributes (audit X16). The demo instance has the IT
// infrastructure starter installed; installing on an empty instance is covered by fresh-install.spec.ts.

test("starter templates: head band, status pill and template panels", async ({ page }, testInfo) => {
  await page.goto("/admin/templates");
  const head = page.locator(".record-head-plain");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Starter templates");
  await expect(head.getByRole("heading", { level: 1, name: "Starter templates" })).toBeVisible();
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText(/^\d+ templates?$/);
  await expect(meta.locator(".badge.ok")).toHaveText(/^\d+ of \d+ installed$/);

  const panel = page.getByRole("region", { name: "IT infrastructure" });
  await expect(panel.locator(".panel-header .badge")).toHaveText("Installed");
  await expect(panel.locator(".panel-header .badge .status-dot")).toBeVisible();
  await expect(panel.getByRole("button", { name: "Installed" })).toBeDisabled();
  await expect(panel.getByRole("table", { name: "What IT infrastructure contains" })).toBeVisible();
  await expect(panel.getByRole("table", { name: "Classes in IT infrastructure" })).toBeVisible();
  await expect(panel.getByRole("row", { name: /CI classes 8/ })).toBeVisible();
  // The served CSP (style-src 'self') drops style attributes: the page uses classes only.
  await expect(page.locator(".record-head-plain [style], .panel [style]")).toHaveCount(0);

  await checkA11y(page, testInfo, "templates-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "templates-dark");
  await chooseTheme(page, "");
});

test("starter templates: the page texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/templates");
  const head = page.locator(".record-head-plain");
  await expect(head.getByRole("heading", { level: 1, name: "Startvorlagen" })).toBeVisible();
  await expect(head.getByTestId("record-meta").locator(".badge.ok")).toHaveText(/^\d+ von \d+ installiert$/);
  const panel = page.getByRole("region", { name: "IT infrastructure" });
  await expect(panel.locator(".panel-header .badge")).toHaveText("Installiert");
  await expect(panel.getByRole("button", { name: "Installiert" })).toBeDisabled();
  await expect(panel.getByRole("columnheader", { name: "Bereits vorhanden" })).toBeVisible();
  await expect(panel.getByRole("rowheader", { name: "CI-Klassen" })).toBeVisible();
  await expect(page.getByText(/Erneutes Installieren ergänzt nur/).first()).toBeVisible();
});
