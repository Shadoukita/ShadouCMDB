import { checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Audit log in the reference-mockup look (design document §0, step 12f-5): the inventory's head
// band (breadcrumb, title with the count, intro, actor search, record type, action and source chips) above the
// table card, event pills, mono teal record links and numbered pages. Read-only.

test("audit log: head band, event pills, mono record links, numbered pages", async ({ page }, testInfo) => {
  await page.goto("/admin/audit?entityType=configuration_items");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Audit log");
  await expect(head.getByRole("heading", { level: 1, name: "Audit log" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ entr(y|ies)$/);
  const search = head.getByRole("search");
  await expect(search.getByLabel("Actor name")).toBeVisible();
  await expect(search.getByLabel("Record type")).toHaveValue("configuration_items");
  await expect(search.getByRole("group", { name: "Filter by source" }).getByRole("button", { name: "UI" })).toBeVisible();

  const list = page.getByRole("region", { name: "Audit log" });
  await expect(list.getByRole("search")).toHaveCount(0);
  await expect(list.locator("table.list-table th").first()).toHaveCSS("text-transform", "none");
  const row = list.locator("tbody tr").first();
  await expect(row.locator(".badge")).toHaveCSS("border-radius", "999px");
  await expect(row.getByRole("link").last()).toHaveClass(/\blist-name\b/);
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  await checkA11y(page, testInfo, "admin-audit-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-audit-dark");
  await chooseTheme(page, "");
});

test("audit log: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/audit");
  await expect(page.locator(".list-head").getByRole("heading", { level: 1, name: "Audit-Protokoll" })).toBeVisible();
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ Eintr(ag|äge)$/);
  await expect(page.getByRole("columnheader", { name: "Ereignis" })).toBeVisible();
});
