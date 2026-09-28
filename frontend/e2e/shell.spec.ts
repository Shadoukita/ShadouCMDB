import { snap, expect, test } from "./support";

test("dashboard shows server-side counts and the class nav has live counts", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await expect(page.locator(".kpi .value")).toHaveText(/^\d[\d,.]*$/);
  await expect(page.getByRole("heading", { name: "By class", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "By status", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Recently changed", exact: true })).toBeVisible();

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
