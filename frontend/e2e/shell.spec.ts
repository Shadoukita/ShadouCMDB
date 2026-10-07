import { snap, expect, resetUiSettings, test } from "./support";

test("dashboard shows server-side counts and the class nav has live counts", async ({ page, request }) => {
  await resetUiSettings(request); // the built-in widgets, not a customized dashboard
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  const stats = page.getByRole("region", { name: "Dashboard figures" });
  await expect(stats.locator('[data-stat="total"] .value')).toHaveText(/^\d[\d,.]*$/);
  await expect(stats.locator('[data-stat="total"] .note')).toHaveText(/^in \d+ class(es)?$/);
  await expect(stats.locator('[data-stat="changes"] .label')).toHaveText("Changes (7 days)");
  await expect(stats.locator('[data-stat="services"] .value')).toHaveText(/^\d[\d,.]*$/);
  // The built-in widgets render through the same grid as customized ones (audit D1).
  const byClass = page.locator('[data-widget="by_class"]');
  await expect(byClass.getByRole("heading", { name: "CIs by class", exact: true })).toBeVisible();
  await expect(byClass.getByRole("columnheader")).toHaveText(["Class", "CIs", "Share", "Actions"]);
  await expect(byClass.getByRole("row", { name: /^Server\b/ }).getByRole("link", { name: "New Server" })).toBeVisible();
  await expect(page.locator('[data-widget="by_status"]').getByRole("heading", { name: /^CIs by / })).toBeVisible();
  await expect(page.locator('[data-widget="recent"]').getByRole("heading", { name: "Recently changed", exact: true })).toBeVisible();

  const nav = page.getByRole("navigation", { name: "Main" });
  const serverLink = nav.getByRole("link", { name: /^Server \d+$/ });
  await expect(serverLink).toBeVisible(); // name includes the count once it has loaded
  await snap(page, "01-dashboard");

  await serverLink.click();
  await expect(page).toHaveURL(/\/cis\?classId=/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Server");
  await expect(serverLink).toHaveAttribute("aria-current", "page");
  await expect(nav.getByRole("link", { name: "All configuration items" })).not.toHaveAttribute("aria-current", "page");
});

test("global search: '/' focuses it, type-ahead finds by IP, Enter opens results", async ({ page }) => {
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await page.keyboard.press("/");
  await expect(page.locator("#global-search")).toBeFocused();
  // crm-app-01 holds this address in its ip_address attribute.
  await page.keyboard.type("10.20.5.21");
  await expect(page.getByRole("option", { name: /crm-app-01/ })).toBeVisible();
  await snap(page, "02-global-search-typeahead");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/search\?q=10\.20\.5\.21/);
  await expect(page.getByRole("link", { name: "crm-app-01", exact: true })).toBeVisible();
  await expect(page.getByRole("cell", { name: /IP address: 10\.20\.5\.21/ })).toBeVisible();
});
