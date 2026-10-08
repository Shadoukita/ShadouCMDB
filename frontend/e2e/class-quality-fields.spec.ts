import { apiGet, apiSend, expect, test } from "./support";

// A class's data-quality fields (the dashboard's "Without an owner" and "End of life" checks): the class page
// offers only fields of the types each check accepts, inherited ones included; a subtype without a setting of
// its own shows the parent's; the API's refusal shows next to the select; once set, the checks count the
// class's CIs. The classes are built through the API with field keys migration 0068 does not recognise.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PARENT = `Quality parent ${stamp}`;
const CHILD = `Quality child ${stamp}`;
let parentId = "";
let childId = "";
const ids: Record<string, string> = {};

interface Page_<T> {
  data: T[];
}
const qualityList = (request: Parameters<typeof apiGet>[0], check: string) =>
  apiGet<Page_<{ id: string }>>(request, `/configuration-items?classId=${childId}&quality=${check}&limit=50`);

test.beforeAll(async ({ request }) => {
  parentId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `quality_parent_${stamp}`, name: PARENT })).id;
  childId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `quality_child_${stamp}`, name: CHILD, parentId })).id;
  const attr = (classId: string, body: Record<string, unknown>) => apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, ...body });
  ids.steward = (await attr(parentId, { key: "steward", label: "Steward", dataType: "text", sortOrder: 10 })).id;
  ids.retires = (await attr(parentId, { key: "retires_on", label: "Retires on", dataType: "date", sortOrder: 20 })).id;
  ids.cores = (await attr(parentId, { key: "cores", label: "Cores", dataType: "integer", sortOrder: 30 })).id;
  ids.custodian = (await attr(childId, { key: "custodian", label: "Custodian", dataType: "text", sortOrder: 40 })).id;
  ids.code = (await attr(childId, { key: "code", label: "Code", dataType: "text", sortOrder: 50 })).id;
  await apiSend(request, "PATCH", `/ci-classes/${childId}`, { titleAttributeId: ids.code });
});

test("the parent's owner and end-of-life fields are set from the class page", async ({ page, request }) => {
  await page.goto(`/admin/classes/${parentId}`);
  const owner = page.getByLabel("Owner field");
  const eol = page.getByLabel("End-of-life field");
  // Only the types each check accepts: no integer field, no date field as owner, no text field as end of life.
  await expect(owner.locator("option")).toHaveText(["None – check off for this class", "Steward (steward)"]);
  await expect(eol.locator("option")).toHaveText(["None – check off for this class", "Retires on (retires_on)"]);
  await owner.selectOption({ label: "Steward (steward)" });
  await eol.selectOption({ label: "Retires on (retires_on)" });
  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  const cls = await apiGet<{ ownerAttributeId: string | null; endOfLifeAttributeId: string | null }>(request, `/ci-classes/${parentId}`);
  expect(cls.ownerAttributeId).toBe(ids.steward);
  expect(cls.endOfLifeAttributeId).toBe(ids.retires);

  // After a reload the class page shows what is stored.
  await page.reload();
  await expect(owner.locator("option:checked")).toHaveText("Steward (steward)");
  await expect(eol.locator("option:checked")).toHaveText("Retires on (retires_on)");
});

test("a subtype shows the inherited setting, offers inherited fields and overrides it", async ({ page, request }) => {
  await page.goto(`/admin/classes/${childId}`);
  const owner = page.getByLabel("Owner field");
  const eol = page.getByLabel("End-of-life field");
  await expect(owner.locator("option:checked")).toHaveText("Inherited: Steward (steward)");
  await expect(eol.locator("option:checked")).toHaveText("Inherited: Retires on (retires_on)");
  await expect(page.getByTestId("owner-inherited")).toHaveText(`Inherited from ${PARENT}: Steward (steward)`);
  await expect(page.getByTestId("end-of-life-inherited")).toHaveText(`Inherited from ${PARENT}: Retires on (retires_on)`);
  await expect(owner.locator("option")).toHaveText([
    "Inherited: Steward (steward)",
    `Steward (steward) · from ${PARENT}`,
    "Custodian (custodian)",
    "Code (code)",
  ]);

  // The API's check shows next to the select: the chosen field stopped being a text field before the save.
  await owner.selectOption({ label: "Custodian (custodian)" });
  await apiSend(request, "PATCH", `/attribute-definitions/${ids.custodian}`, { dataType: "integer" });
  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.locator("#class-owner-err")).toBeVisible();
  await expect(owner).toHaveAttribute("aria-invalid", "true");
  await apiSend(request, "PATCH", `/attribute-definitions/${ids.custodian}`, { dataType: "text" });

  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  await expect(owner).not.toHaveAttribute("aria-invalid", "true");
  // Its own setting replaces the inherited line; the end of life still comes from the parent.
  await expect(page.getByTestId("owner-inherited")).toHaveCount(0);
  await expect(page.getByTestId("end-of-life-inherited")).toBeVisible();
  const cls = await apiGet<{ ownerAttributeId: string | null; endOfLifeAttributeId: string | null }>(request, `/ci-classes/${childId}`);
  expect(cls.ownerAttributeId).toBe(ids.custodian);
  expect(cls.endOfLifeAttributeId).toBeNull();
});

test("the checks count the class's CIs by the fields set in the UI", async ({ request }) => {
  const past = new Date(Date.now() - 86_400_000).toISOString().slice(0, 10);
  const far = new Date(Date.now() + 3 * 365 * 86_400_000).toISOString().slice(0, 10);
  const ci = (attributes: Record<string, unknown>) =>
    apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: childId, attributes }).then((r) => r.id);
  const unowned = await ci({ code: `dq-a-${stamp}`, retires_on: past });
  const owned = await ci({ code: `dq-b-${stamp}`, custodian: "Ops team", retires_on: far });

  const quality = await apiGet<{ checks: { key: string; configured: boolean }[] }>(request, "/configuration-items/data-quality");
  expect(quality.checks.find((c) => c.key === "no_owner")?.configured).toBe(true);
  expect(quality.checks.find((c) => c.key === "end_of_life")?.configured).toBe(true);
  // The subtype's own owner field (Custodian) and the inherited end-of-life field (Retires on) decide.
  expect((await qualityList(request, "no_owner")).data.map((c) => c.id)).toEqual([unowned]);
  expect((await qualityList(request, "end_of_life")).data.map((c) => c.id)).toEqual([unowned]);
  expect(owned).toBeTruthy();
});

test("the dashboard counts the checks after a reload, and its 'checks are off' note links to the classes", async ({ page, request }) => {
  // The fields set in the UI above turn both checks on: after a reload the panel lists them with the API's counts.
  type Check = { key: string; count: number; configured: boolean };
  const { checks } = await apiGet<{ checks: Check[] }>(request, "/configuration-items/data-quality");
  await page.goto("/");
  const panel = page.getByRole("region", { name: "Needs attention" });
  for (const key of ["no_owner", "end_of_life"]) {
    const c = checks.find((x) => x.key === key)!;
    expect(c.configured).toBe(true);
    expect(c.count).toBeGreaterThanOrEqual(1);
    await expect(panel.locator(`[data-check="${key}"] .attention-count`)).toHaveText(c.count.toLocaleString("en-US"));
  }

  // While a check is off, a data-model administrator is told so and sent to the classes, where the fields are set.
  await page.route("**/api/v1/configuration-items/data-quality", async (route) => {
    const body = await (await route.fetch()).json();
    for (const c of body.checks) if (c.key === "no_owner" || c.key === "end_of_life") c.configured = false;
    await route.fulfill({ json: body });
  });
  await page.reload();
  await expect(panel.locator('[data-check="no_owner"]')).toHaveCount(0);
  const note = panel.locator(".attention-off");
  await expect(note).toContainText("2 checks are off");
  await note.getByRole("link", { name: "Set the fields in the classes" }).click();
  await expect(page).toHaveURL(/\/admin\/classes$/);
});
