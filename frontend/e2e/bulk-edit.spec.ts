import { apiGet, classIdByName, createCi, csrf, expect, expectDialogLaidOut, snap, test } from "./support";

// The inventory's bulk edit (gap G10, SHAA-2586): the selection's Bulk edit opens a form of the fields to
// change, sends one POST /configuration-items/bulk-update, then reports how many CIs were updated and lists
// each refused one with the API's reason. The list refreshes, and the refused CIs stay selected.

const created: string[] = [];

test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of created.splice(0)) await request.delete(`/api/v1/configuration-items/${id}`, { headers });
});

test("bulk edit: one request, the count of updated CIs, the refused ones with their reason", async ({ page, request }) => {
  const stamp = `e2e-bulk-${Date.now().toString(36)}`;
  const serverId = await classIdByName(request, "Server");
  const ids: string[] = [];
  for (const n of ["a", "b", "c"]) ids.push((await createCi(request, serverId, `${stamp}-srv-${n}`)).id);
  created.push(...ids);

  await page.goto(`/cis?q=${stamp}&classId=${serverId}&columns=label,attributes.cpu_cores&sort=label`);
  const rows = page.locator("table.data tbody tr");
  await expect(rows).toHaveCount(3);
  await page.getByRole("checkbox", { name: "Select all rows on this page" }).check();
  const footer = page.locator(".table-footer");
  await expect(footer.getByRole("status")).toHaveText("3 selected");

  await footer.getByRole("button", { name: "Bulk edit" }).click();
  const dialog = page.getByRole("dialog", { name: "Bulk edit 3 CIs" });
  await expectDialogLaidOut(dialog);
  const apply = dialog.getByRole("button", { name: "Apply to 3 CIs" });
  await expect(apply).toBeDisabled();

  // A value the class refuses is reported per CI, next to its field.
  await dialog.getByLabel("Field to change").selectOption({ label: "CPU cores" });
  await dialog.getByRole("button", { name: "Add", exact: true }).click();
  await dialog.getByLabel("CPU cores", { exact: true }).fill("0");
  const requests: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/v1/configuration-items/bulk-update")) requests.push(r.method());
  });
  await apply.click();
  const result = page.getByRole("dialog", { name: "Bulk edit result" });
  await expect(result.getByRole("status")).toHaveText("0 CIs of 3 updated.");
  await expect(result.getByRole("heading", { name: "Refused (3)" })).toBeVisible();
  await expect(result.locator(".bulk-refused > li")).toHaveCount(3);
  await expect(result.locator(".bulk-refused > li").first()).toContainText(`${stamp}-srv-a`);
  await expect(result.locator(".bulk-refused > li").first()).toContainText("CPU cores:");
  await result.getByRole("button", { name: "Close" }).click();
  await expect(footer.getByRole("status")).toHaveText("3 selected");

  // One CI disappears behind the operator's back: the others are updated, it is refused as not found.
  const del = await request.delete(`/api/v1/configuration-items/${ids[2]}`, { headers: { "X-CSRF-Token": await csrf(request) } });
  expect(del.ok()).toBeTruthy();
  await footer.getByRole("button", { name: "Bulk edit" }).click();
  await dialog.getByLabel("Field to change").selectOption({ label: "CPU cores" });
  await dialog.getByRole("button", { name: "Add", exact: true }).click();
  await dialog.getByLabel("CPU cores", { exact: true }).fill("16");
  await dialog.getByLabel("Field to change").selectOption({ label: "Criticality" });
  await dialog.getByRole("button", { name: "Add", exact: true }).click();
  await expect(dialog.getByLabel("Criticality", { exact: true })).toHaveValue("");
  await expect(dialog.getByText("Left empty, this value is cleared on every selected CI.")).toBeVisible();
  await snap(page, "bulk-edit-form");
  await dialog.getByRole("button", { name: "Do not change Criticality" }).click();
  await dialog.getByRole("button", { name: "Apply to 3 CIs" }).click();
  await expect(result.getByRole("status")).toHaveText("2 CIs of 3 updated.");
  await expect(result.locator(".bulk-refused > li")).toHaveCount(1);
  await expect(result.locator(".bulk-refused > li")).toContainText(`${stamp}-srv-c`);
  await expect(result.getByRole("link", { name: `${stamp}-srv-c` })).toHaveAttribute("href", `/cis/${ids[2]}`);
  await snap(page, "bulk-edit-result");
  await result.getByRole("button", { name: "Close" }).click();
  expect(requests).toEqual(["POST", "POST"]);

  // The list refreshed (the deleted CI is gone, the new value shows); the updated CIs left the selection. The
  // deleted one stays: it can still be restored (CONFLICT); only CIs that no longer exist (NOT_FOUND) leave it.
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0).locator("td").nth(2)).toHaveText("16");
  await expect(rows.nth(1).locator("td").nth(2)).toHaveText("16");
  await expect(footer.getByRole("status")).toHaveText("1 selected");
  await snap(page, "bulk-edit-after");
  const stored = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${ids[0]}`);
  expect(stored.attributes.cpu_cores).toBe(16);
});

test("bulk edit without a class filter offers only the criticality", async ({ page, request }) => {
  const stamp = `e2e-bulk2-${Date.now().toString(36)}`;
  const serverId = await classIdByName(request, "Server");
  const appId = await classIdByName(request, "Application");
  created.push((await createCi(request, serverId, `${stamp}-srv`)).id, (await createCi(request, appId, `${stamp}-app`)).id);

  await page.goto(`/cis?q=${stamp}`);
  await expect(page.locator("table.data tbody tr")).toHaveCount(2);
  await page.getByRole("checkbox", { name: "Select all rows on this page" }).check();
  await page.locator(".table-footer").getByRole("button", { name: "Bulk edit" }).click();
  const dialog = page.getByRole("dialog", { name: "Bulk edit 2 CIs" });
  await expect(dialog.getByText("Filter the list to one class to edit its attributes.", { exact: false })).toBeVisible();
  await expect(dialog.getByLabel("Field to change").locator("option")).toHaveText(["Choose a field…", "Criticality"]);
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
});
