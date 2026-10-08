import { apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Dropdowns in the reference-mockup look (design document §0, step 12f-8): the inventory's head band
// (breadcrumb, title with the count, New list, intro) above the lookup lists, mono teal names and state pills; the
// selected list's values use the same table headers, mono teal names and pills.
test.describe.configure({ mode: "serial" });

const STAMP = Date.now().toString(36);
const PARENT = `E2E look maker ${STAMP}`;
const CHILD = `E2E look model ${STAMP}`;
const VALUE = `E2E look value ${STAMP}`;
let childId = "";

test.beforeAll(async ({ request }) => {
  const parent = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `e2e_look_maker_${STAMP}`, name: PARENT });
  const child = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", {
    key: `e2e_look_model_${STAMP}`,
    name: CHILD,
    parentListId: parent.id,
  });
  childId = child.id;
  // A value of a dependent list needs a parent value.
  const maker = await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", {
    listId: parent.id,
    key: `e2e_look_maker_${STAMP}`,
    name: `E2E look maker value ${STAMP}`,
    sortOrder: 10,
  });
  await apiSend(request, "POST", "/lookup-list-values", {
    listId: childId,
    key: `e2e_look_value_${STAMP}`,
    name: VALUE,
    parentValueId: maker.id,
    sortOrder: 10,
  });
});

test("dropdowns: head band, mono names, state pills, values table", async ({ page }, testInfo) => {
  await page.goto(`/admin/dropdowns?list=${childId}`);
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Dropdowns");
  await expect(head.getByRole("heading", { level: 1, name: "Dropdowns" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("button", { name: "New list" })).toBeVisible();
  await expect(head.locator(".page-intro")).toBeVisible();

  const lists = page.getByRole("region", { name: "Lookup lists" });
  await expect(lists.locator("table.lookup-lists")).toHaveClass(/\blist-table\b/);
  await expect(lists.locator("table.list-table th").first()).toHaveCSS("text-transform", "none");
  const row = lists.getByRole("row").filter({ has: page.getByRole("link", { name: CHILD, exact: true }) });
  await expect(row.getByRole("link", { name: CHILD, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.getByRole("link", { name: PARENT, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(row.locator(".badge.ok")).toHaveCSS("border-radius", "999px");

  const values = page.getByRole("region", { name: `Values of “${CHILD}”` });
  await expect(values.locator("table.reorderable")).toHaveClass(/\blist-table\b/);
  const valueRow = values.getByRole("row").filter({ hasText: VALUE });
  await expect(valueRow.getByRole("button", { name: VALUE, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(valueRow.locator(".badge.ok .status-dot")).toHaveCount(1);

  await checkA11y(page, testInfo, "admin-dropdowns-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-dropdowns-dark");
  await chooseTheme(page, "");
});

test("dropdowns: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto(`/admin/dropdowns?list=${childId}`);
  await expect(page.locator(".list-head").getByRole("heading", { level: 1 })).toHaveText("Auswahllisten");
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(page.locator(".list-head").getByRole("button", { name: "Neue Liste" })).toBeVisible();
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: CHILD, exact: true }) });
  await expect(row.locator(".badge.ok")).toHaveText("Aktiv");
});
