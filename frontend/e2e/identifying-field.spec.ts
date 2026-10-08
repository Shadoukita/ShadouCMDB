import { apiGet, apiSend, classIdByName, expect, snap, test } from "./support";

// Identifying and expected fields (GH#763): migration 0069 leaves every existing field
// non-identifying, and the release note sends administrators to the data model to mark their
// serial numbers and asset tags. The attribute dialog sets and clears both flags, and the
// attribute list marks an identifying field.
const stamp = Date.now().toString(36);

interface Attr {
  id: string;
  key: string;
  isExpected: boolean;
  isIdentifying: boolean;
}

test("the attribute dialog marks a field as identifying and expected, and clears it again", async ({ page, request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", {
    key: `e2e_ident_${stamp}`,
    name: `E2E identifying ${stamp}`,
    parentId: await classIdByName(request, "Hardware"),
  });
  // Hardware already defines serial_number, so the subclass gets a field of its own.
  const key = `asset_code_${stamp}`;
  const label = `Asset code ${stamp}`;
  await apiSend(request, "POST", "/attribute-definitions", { classId: cls.id, key, label, dataType: "text" });
  const field = async () => (await apiGet<{ data: Attr[] }>(request, `/ci-classes/${cls.id}/attributes`)).data.find((a) => a.key === key);
  expect(await field()).toMatchObject({ isExpected: false, isIdentifying: false });

  await page.goto(`/admin/classes/${cls.id}`);
  const row = page.locator("table.attributes tr", { has: page.getByRole("button", { name: label, exact: true }) });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toHaveCount(0);

  await row.getByRole("button", { name: label, exact: true }).click();
  await expect(page.getByLabel("Identifies one CI (not copied when cloning)")).not.toBeChecked();
  await page.getByLabel("Identifies one CI (not copied when cloning)").check();
  await page.getByLabel("Counts towards record completeness (not enforced on save)").check();
  await snap(page, "identifying-dialog");
  await page.getByRole("button", { name: "Save attribute" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved attribute ${label}.` })).toBeVisible();
  await expect.poll(field).toMatchObject({ isExpected: true, isIdentifying: true });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toBeVisible();

  // Reopened, the dialog shows the stored flags; cleared, only the changed one is sent.
  await row.getByRole("button", { name: label, exact: true }).click();
  await expect(page.getByLabel("Identifies one CI (not copied when cloning)")).toBeChecked();
  await expect(page.getByLabel("Counts towards record completeness (not enforced on save)")).toBeChecked();
  await page.getByLabel("Identifies one CI (not copied when cloning)").uncheck();
  await page.getByRole("button", { name: "Save attribute" }).click();
  // The toast of the first save may still be showing, so wait for the stored flags instead.
  await expect.poll(field).toMatchObject({ isExpected: true, isIdentifying: false });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toHaveCount(0);
});
