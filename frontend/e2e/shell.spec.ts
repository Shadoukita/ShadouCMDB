import { apiSend, checkA11y, openUserMenu, snap, expect, resetUiSettings, test } from "./support";

test("dashboard shows server-side counts and the class nav has live counts", async ({ page, request }) => {
  await resetUiSettings(request); // the built-in widgets, not a customized dashboard
  await page.goto("/");
  await expect(page).toHaveTitle(/^Dashboard · /);
  await expect(page.getByRole("heading", { level: 1, name: /^Dashboard: Good (morning|afternoon|evening)$/ })).toBeVisible();
  const stats = page.getByRole("region", { name: "Dashboard figures" });
  await expect(stats.locator('[data-stat="total"] .value')).toHaveText(/^\d[\d,.]*$/);
  await expect(stats.locator('[data-stat="relationships"] .value')).toHaveText(/^\d[\d,.]*$/);
  await expect(stats.locator('[data-stat="changes"] .label')).toHaveText("Changes this period");
  await expect(stats.locator('[data-stat="complete"] .value')).toHaveText(/^\d[\d,.]*%$/);
  // The built-in widgets render through the same grid as customized ones (audit D1), after the changes chart.
  await expect(page.locator('[data-widget="changes"]').getByRole("heading", { name: "Changes over 14 days" })).toBeVisible();
  const byClass = page.locator('[data-widget="by_class"]');
  await expect(byClass.getByRole("heading", { name: "CIs by class", exact: true })).toBeVisible();
  await expect(byClass.getByRole("listitem").filter({ has: page.getByRole("link", { name: "Server", exact: true }) }).getByRole("link", { name: "New Server" })).toBeVisible();
  await expect(page.locator('[data-widget="by_status"]').getByRole("heading", { name: /^CIs by / })).toBeVisible();
  const recent = page.locator('[data-widget="recent"]');
  await expect(recent.getByRole("heading", { name: "Recent activity", exact: true })).toBeVisible();
  await expect(recent.getByRole("columnheader")).toHaveText(["Configuration item", "Class", "Change", "By", "When"]);

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

// Step 12b (design §0): the rail's sections, the mono Inventory count, saved views as links, the user block.
test("rail: Workspace, Administration and Saved views sections; a saved view opens from the rail", async ({ page, request }, testInfo) => {
  await resetUiSettings(request);
  const name = `E2E rail view ${Date.now().toString(36)}`;
  const view = await apiSend<{ id: string }>(request, "POST", "/saved-views", {
    context: "inventory",
    name,
    visibility: "personal",
    definition: { classKeys: ["server"] },
  });
  await page.goto("/");
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("heading", { level: 2 })).toContainText(["Workspace", "Administration", "Saved views"]);
  // The count stays out of the link's name; the exact figure is its tooltip.
  const inventory = nav.getByRole("link", { name: "All configuration items", exact: true });
  await expect(inventory.locator(".nav-count")).toHaveText(/^\d[\d.,]*\s?[KkMT]?$/);
  await expect(inventory.locator(".nav-count")).toHaveAttribute("title", /^\d[\d,.]* configuration items$/);
  await expect(nav.getByRole("button", { name: /^Signed in as \S/ })).toBeVisible();
  // Each saved view shows the CIs it lists (GET /saved-views/counts, gap G4), outside the link's name like the Inventory count.
  const viewCount = nav.getByRole("link", { name, exact: true }).locator(".nav-count");
  await expect(viewCount).toHaveText(/^\d[\d,]*$/);
  await expect(viewCount).toHaveAttribute("title", /^\d[\d,]* configuration items$/);
  await checkA11y(page, testInfo, "shell-rail", { include: "#shell-nav" });
  await snap(page, "12b-shell-rail");

  const link = nav.getByRole("link", { name, exact: true });
  await link.click();
  await expect(page).toHaveURL(new RegExp(`/cis\\?.*view=${view.id}`));
  await expect(link).toHaveAttribute("aria-current", "page");
  // The view's class and the inventory do not light up as well.
  await expect(nav.locator('a[aria-current="page"]')).toHaveCount(1);

  // The menu opens upwards over the dark rail but keeps its light surface: the rail's link styles stay out of it.
  await openUserMenu(page);
  await expect(page.locator(".user-menu-panel")).toBeVisible();
  await checkA11y(page, testInfo, "shell-user-block-menu", { include: ".user-menu-panel" });

  const csrf = (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
  await request.delete(`/api/v1/saved-views/${view.id}?version=1`, { headers: { "X-CSRF-Token": csrf } });
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
