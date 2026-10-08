import { checkA11y, chooseTheme, classIdByName, createCi, csrf, expect, test, withInventoryFilters } from "./support";

// The inventory in the reference-mockup look (design document §0, step 12c): "x of y" in the title,
// removable chips for every filter, Add filter, Save view, the checkbox column with "n selected" and a
// Bulk edit that says why it is not available yet (gap G10), and numbered pages.

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

  // Names are mono links; with no class column the class is the subtitle (until gap G8).
  await page.goto(`/cis?q=${stamp}&classId=${serverId}&columns=label,ident`);
  const first = rows.first();
  await expect(first.locator(".ci-name")).toHaveText(`${stamp}-srv-a`);
  await expect(first.locator(".ci-subtitle")).toHaveText("Server");

  // Selection: per row and per page, counted in the footer; Bulk edit says why it is not available.
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
  await expect(bulk).toHaveAttribute("aria-disabled", "true");
  await expect(bulk).toHaveAccessibleDescription("Editing several CIs at once is not available yet.");
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
