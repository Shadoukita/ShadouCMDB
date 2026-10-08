import { apiGet, apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Relationship types in the reference-mockup look (design document §0, step 12f-7): the inventory's
// head band (breadcrumb, title with the count, New relationship type, intro) above the type list, mono teal names and
// state pills; the selected type's rules use the same table headers and mono teal class links.
test.describe.configure({ mode: "serial" });

const STAMP = Date.now().toString(36);
const NAME = `E2E look ${STAMP}`;
let typeId = "";

test.beforeAll(async ({ request }) => {
  const rt = await apiSend<{ id: string }>(request, "POST", "/relationship-types", {
    key: `e2e_look_${STAMP}`,
    name: NAME,
    forwardLabel: "looks at",
    reverseLabel: "is looked at by",
    isDirectional: true,
    impactDirection: "none",
    sortOrder: 9990,
  });
  typeId = rt.id;
  const classes = await apiGet<{ data: { id: string; isActive: boolean; isAbstract: boolean }[] }>(request, "/ci-classes?limit=200");
  const [a, b] = classes.data.filter((c) => c.isActive && !c.isAbstract);
  await apiSend(request, "POST", "/relationship-rules", { relationshipTypeId: typeId, sourceClassId: a.id, targetClassId: b.id });
});

test("relationship types: head band, mono names, state pills, rules table", async ({ page }, testInfo) => {
  await page.goto(`/admin/relationships?type=${typeId}`);
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Relationship types");
  await expect(head.getByRole("heading", { level: 1, name: "Relationship types" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("button", { name: "New relationship type" })).toBeVisible();
  await expect(head.locator(".page-intro")).toBeVisible();

  const list = page.getByRole("region", { name: "Relationship types" });
  await expect(list.locator("table.list-table th").nth(1)).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row").filter({ has: page.getByRole("link", { name: NAME, exact: true }) });
  await expect(row.getByRole("link", { name: NAME, exact: true })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge.ok").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(row.locator(".badge.ok")).toHaveCSS("border-radius", "999px");

  const rules = page.getByRole("region", { name: `Rules for “${NAME}”` });
  await expect(rules.locator("table.relationship-rules")).toHaveClass(/\blist-table\b/);
  await expect(rules.locator("table.relationship-rules tbody a.list-name")).toHaveCount(2);

  await checkA11y(page, testInfo, "admin-reltypes-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-reltypes-dark");
  await chooseTheme(page, "");
});

test("relationship types: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto(`/admin/relationships?type=${typeId}`);
  await expect(page.locator(".list-head").getByRole("heading", { level: 1 })).toHaveText("Beziehungstypen");
  await expect(page.locator(".list-head .count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(page.locator(".list-head").getByRole("button", { name: "Neuer Beziehungstyp" })).toBeVisible();
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: NAME, exact: true }) });
  await expect(row.locator(".badge.ok")).toHaveText("Aktiv");
});
