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
  await apiSend(request, "POST", "/attribute-definitions", { classId: cls.id, key: "serial_number", label: "Serial number", dataType: "text" });
  const serial = async () => (await apiGet<{ data: Attr[] }>(request, `/ci-classes/${cls.id}/attributes`)).data.find((a) => a.key === "serial_number");
  expect(await serial()).toMatchObject({ isExpected: false, isIdentifying: false });

  await page.goto(`/admin/classes/${cls.id}`);
  const row = page.locator("table.attributes tr", { has: page.getByRole("button", { name: "Serial number", exact: true }) });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toHaveCount(0);

  await row.getByRole("button", { name: "Serial number", exact: true }).click();
  await expect(page.getByLabel("Identifies one CI (not copied when cloning)")).not.toBeChecked();
  await page.getByLabel("Identifies one CI (not copied when cloning)").check();
  await page.getByLabel("Counts towards record completeness (not enforced on save)").check();
  await snap(page, "identifying-dialog");
  await page.getByRole("button", { name: "Save attribute" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved attribute Serial number." })).toBeVisible();
  expect(await serial()).toMatchObject({ isExpected: true, isIdentifying: true });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toBeVisible();

  // Reopened, the dialog shows the stored flags; cleared, only the changed one is sent.
  await row.getByRole("button", { name: "Serial number", exact: true }).click();
  await expect(page.getByLabel("Identifies one CI (not copied when cloning)")).toBeChecked();
  await expect(page.getByLabel("Counts towards record completeness (not enforced on save)")).toBeChecked();
  await page.getByLabel("Identifies one CI (not copied when cloning)").uncheck();
  await page.getByRole("button", { name: "Save attribute" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved attribute Serial number." })).toBeVisible();
  expect(await serial()).toMatchObject({ isExpected: true, isIdentifying: false });
  await expect(row.locator(".badge", { hasText: "Identifying" })).toHaveCount(0);
});
