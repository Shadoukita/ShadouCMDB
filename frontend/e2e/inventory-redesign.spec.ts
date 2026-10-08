import { checkA11y, chooseTheme, classIdByName, createCi, csrf, expect, test, withInventoryFilters } from "./support";

// The inventory in the reference-mockup look (design document §0, step 12c): "x of y" in the title,
// removable chips for every filter, Add filter, Save view, the checkbox column with "n selected" and
// Bulk edit (gap G10, bulk-edit.spec.ts), and numbered pages.

// Deleted afterwards: "e2e-inv-…" sorts before the demo servers, which later specs open by label.
const created: string[] = [];

test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of created.splice(0)) {
    const res = await request.delete(`/api/v1/configuration-items/${id}`, { headers });
    expect(res.ok(), `delete ${id} → ${res.status()}`).toBeTruthy();
  }
});

test("inventory: filter chips, Add filter, row selection and page numbers", async ({ page, request }, testInfo) => {
  const stamp = `e2e-inv-${Date.now().toString(36)}`;
  const serverId = await classIdByName(request, "Server");
  const appId = await classIdByName(request, "Application");
  for (const n of ["a", "b", "c"]) created.push((await createCi(request, serverId, `${stamp}-srv-${n}`)).id);
  created.push((await createCi(request, appId, `${stamp}-app`)).id);

  await page.goto(`/cis?q=${stamp}&classId=${serverId}`);
  const rows = page.locator("table.data tbody tr");
  await expect(rows).toHaveCount(3);
  // A filtered list counts against every CI the user may view.
  await expect(page.locator(".page-header .count")).toHaveText(/^3 of [\d,]+$/);

  // Every filter is a chip; removing one removes its URL parameter.
  const chips = page.getByRole("group", { name: "Applied filters" });
  await expect(chips.locator(".chip")).toHaveText([/Class:\s*Server/]);
  await chips.getByRole("button", { name: "Remove the filter Class: Server" }).click();
  await expect(page).not.toHaveURL(/classId=/);
  await expect(rows).toHaveCount(4);

  // Add filter holds the selects; choosing one adds its chip.
  await withInventoryFilters(page, async () => {
    await page.locator("#f-class").selectOption({ label: "Server" });
    await page.locator("#f-active").selectOption("all");
  });
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(chips.locator(".chip")).toHaveText([/Class:\s*Server/, /Validity:\s*Show inactive/]);
  await expect(rows).toHaveCount(3);

  // Names are mono links; with no class column and no subtitle field (gap G8) the class is the subtitle.
  await page.goto(`/cis?q=${stamp}&classId=${serverId}&columns=label,ident`);
  const first = rows.first();
  await expect(first.locator(".ci-name")).toHaveText(`${stamp}-srv-a`);
  await expect(first.locator(".ci-subtitle")).toHaveText("Server");

  // Selection: per row and per page, counted in the footer, with Bulk edit available.
  const footer = page.locator(".table-footer");
  // From a row's checkbox ↓/↑ move to the neighbouring row's checkbox and Space ticks it (GH#750).
  const boxA = rows.nth(0).getByRole("checkbox", { name: `Select ${stamp}-srv-a` });
  const boxB = rows.nth(1).getByRole("checkbox", { name: `Select ${stamp}-srv-b` });
  await boxA.focus();
  await page.keyboard.press("ArrowDown");
  await expect(boxB).toBeFocused();
  await page.keyboard.press("Space");
  await expect(boxB).toBeChecked();
  await page.keyboard.press("ArrowUp");
  await expect(boxA).toBeFocused();
  await rows.nth(0).getByRole("checkbox", { name: `Select ${stamp}-srv-a` }).check();
  await rows.nth(1).getByRole("checkbox", { name: `Select ${stamp}-srv-b` }).check();
  await expect(footer.getByRole("status")).toHaveText("2 selected");
  const all = page.getByRole("checkbox", { name: "Select all rows on this page" });
  expect(await all.evaluate((el) => (el as HTMLInputElement).indeterminate)).toBe(true);
  const bulk = footer.getByRole("button", { name: "Bulk edit" });
  await expect(bulk).not.toHaveAttribute("aria-disabled");
  await all.check();
  await expect(footer.getByRole("status")).toHaveText("3 selected");
  await expect(rows.locator("input[type=checkbox]:checked")).toHaveCount(3);

  // Page numbers: the current page is marked.
  await expect(footer.getByRole("navigation", { name: "Pages" }).getByRole("button", { name: "Page 1" })).toHaveAttribute("aria-current", "page");
  await expect(footer.getByRole("button", { name: "Next page" })).toBeDisabled();

  await checkA11y(page, testInfo, "inventory-light");
  await chooseTheme(page, "dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "inventory-dark");
  await chooseTheme(page, "");

  // Changing the filters clears the selection: it never counts rows the list no longer holds.
  await page.getByRole("button", { name: "Remove the filter Class: Server" }).click();
  await expect(rows).toHaveCount(4);
  await expect(footer.getByRole("status")).toHaveText("");
  await expect(rows.locator("input[type=checkbox]:checked")).toHaveCount(0);
});

test("inventory: Export keeps keyboard focus and says why it is unavailable", async ({ page }) => {
  // Held until checked, so "Exporting…" lasts long enough to look at.
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route("**/api/v1/configuration-items/export?*", async (route) => {
    await held;
    await route.fulfill({ status: 200, contentType: "text/csv", headers: { "Content-Disposition": 'attachment; filename="x.csv"' }, body: '"label"\n' });
  });

  await page.goto("/cis");
  const exportButton = page.locator(".page-header .actions .row-menu > button");
  await expect(exportButton).toHaveText("Export");
  await exportButton.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem", { name: "CSV, comma-separated" })).toBeFocused();
  await page.keyboard.press("Enter");
  // While the export runs the button is aria-disabled, not disabled, so focus stays on it (GH#785).
  await expect(exportButton).toHaveText("Exporting…");
  await expect(exportButton).toHaveAttribute("aria-disabled", "true");
  await expect(exportButton).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menu")).toHaveCount(0);
  release();
  await expect(exportButton).toHaveText("Export");
  await expect(exportButton).toBeFocused();

  // A data-quality drill-down the export cannot apply: reachable by Tab, with the reason as its description (GH#786).
  await page.goto("/cis?quality=no_owner");
  await expect(exportButton).toHaveAttribute("aria-disabled", "true");
  await expect(exportButton).toHaveAccessibleDescription("The export cannot apply the data-quality filter. Remove it to export the list.");
  await exportButton.focus();
  await expect(exportButton).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menu")).toHaveCount(0);
});
