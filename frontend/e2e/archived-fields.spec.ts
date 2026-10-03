import { apiSend, classIdByName, createCi, snap, expect, test } from "./support";

// An archived field keeps its stored values (GH#354: migration 0036 archives the Application field
// "criticality" in favour of the core Criticality). The detail page lists them in their own panel,
// labelled, not among the active fields nor as raw values under "Not defined by this class" (GH#369).
const stamp = Date.now().toString(36);

test("an archived field's value is listed apart on the detail page, labelled as archived", async ({ page, request }) => {
  const classId = await classIdByName(request, "Application");
  const key = `e2e_retired_${stamp}`;
  const attr = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, key, label: `Retired ${stamp}`, dataType: "text" });
  const ci = await createCi(request, classId, `e2e-archived-${stamp}`, { [key]: "kept value" });
  await apiSend(request, "PATCH", `/attribute-definitions/${attr.id}`, { isActive: false });

  await page.goto(`/cis/${ci.id}`);
  const archived = page.locator('[data-section="_archived"]');
  await expect(archived.getByRole("heading", { name: "Archived fields" })).toBeVisible();
  await expect(archived.locator("dt")).toHaveText(`Retired ${stamp} (archived)`);
  await expect(archived.locator("dd")).toHaveText("kept value");
  await expect(page.getByRole("heading", { name: "Not defined by this class" })).toHaveCount(0);
  // Not among the active fields of the layout either.
  await expect(page.locator(".layout-panel").getByText(`Retired ${stamp}`)).toHaveCount(0);
  await expect(page.locator(`#attr-${key}`)).toHaveCount(0);
  await snap(page, "archived-field-detail");
});
