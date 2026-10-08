import { checkA11y, chooseTheme, expect, resetUiSettings, snap, test } from "./support";

// Administration › Customization in the reference-mockup look (design document §0, step 12f-10): the CI page's head
// band (breadcrumb, title with the version in mono, intro) with the section tabs on its lower edge, and the sections'
// tables with the list headers, mono teal names and pills.
test.describe.configure({ mode: "serial" });

test.beforeAll(async ({ request }) => resetUiSettings(request));

test("customization: head band with the section tabs on its edge", async ({ page }, testInfo) => {
  await page.goto("/admin/customization/branding");
  const head = page.locator(".record-head.cust-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Branding");
  await expect(head.getByRole("heading", { level: 1, name: "Customization" })).toBeVisible();
  await expect(head.locator(".page-header .count")).toHaveText(/version \d+/i);
  await expect(head.locator(".page-header .count")).toHaveCSS("font-family", /Plex Mono|mono/i);
  await expect(head.locator(".page-intro")).toBeVisible();
  const tabs = head.getByRole("navigation", { name: "Customization" });
  await expect(tabs).toHaveClass(/\brecord-tabs\b/);
  await expect(tabs.getByRole("link", { name: "Branding" })).toHaveAttribute("aria-current", "page");
  await expect(tabs.getByRole("link", { name: "Branding" })).toHaveCSS("min-height", "40px");
  // The tabs sit on the band's lower edge: no gap between the tab strip and the band's border.
  const band = await head.boundingBox();
  const strip = await tabs.boundingBox();
  expect(band && strip && Math.abs(band.y + band.height - 1 - (strip.y + strip.height))).toBeLessThanOrEqual(1);
  await snap(page, "admin-customization-branding");

  await checkA11y(page, testInfo, "admin-customization-light");
  // Branding has its own Theme field (the default for everyone), so the user menu's is switched from another section.
  await page.goto("/admin/customization/navigation");
  await chooseTheme(page, "dark");
  await page.goto("/admin/customization/branding");
  await expect(head.getByRole("heading", { level: 1, name: "Customization" })).toBeVisible();
  await checkA11y(page, testInfo, "admin-customization-dark");
  await snap(page, "admin-customization-branding-dark");
  await page.goto("/admin/customization/navigation");
  await chooseTheme(page, "");
});

test("customization: layouts and history tables take the list look", async ({ page }, testInfo) => {
  await page.goto("/admin/customization/layouts");
  const templates = page.getByTestId("layout-templates");
  await expect(templates).toHaveClass(/\blist-table\b/);
  await expect(templates.locator("th").first()).toHaveCSS("text-transform", "none");
  const standard = templates.locator("tr[data-template=standard]");
  await expect(standard.locator("th")).toContainText("Standard");
  await expect(standard.locator(".badge").filter({ hasText: "built-in" })).toHaveCSS("border-radius", "999px");
  const classes = page.getByTestId("layout-classes");
  await expect(classes).toHaveClass(/\blist-table\b/);
  await expect(page.locator(".pagination.numbered").first()).toBeVisible();
  await snap(page, "admin-customization-layouts");
  await checkA11y(page, testInfo, "admin-customization-layouts");

  await page.goto("/admin/customization/history");
  const current = page.getByRole("row").filter({ has: page.locator(".badge.ok") });
  await expect(current.locator(".badge.ok .status-dot")).toHaveCount(1);
  await expect(current.locator(".badge.ok")).toHaveCSS("border-radius", "999px");
  await expect(current.locator("td .mono").first()).toHaveText(/^\d+$/);
  await snap(page, "admin-customization-history");
});

test("customization: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/customization/list-views");
  const head = page.locator(".cust-head");
  await expect(head.getByRole("heading", { level: 1 })).toHaveText("Anpassung");
  await expect(head.locator(".page-header .count")).toHaveText(/Version \d+/);
  await expect(head.getByRole("link", { name: "Listenansichten" })).toHaveAttribute("aria-current", "page");
  await expect(head.getByRole("link", { name: "Verlauf" })).toBeVisible();
});
