import { apiGet, apiSend, expect, test } from "./support";

// Redesign gaps G8, G15 and G16 (SHAA-2357, SHAA-2636): a class's subtitle field under a CI's name (inventory and
// CI page head), relationship types grouped by their category on the CI page, and "Updated … by …" from the CI's
// last change, without the actor for a user who lacks audit.view. The admin settings are set through the UI.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const CLASS = `Subtitled ${stamp}`;
const CATEGORY = `Cabling ${stamp}`;
const USERNAME = `e2e-no-audit-${stamp}`;
const PASSWORD = "no-audit-password-123";
let classId = "";
let modelId = "";
let withModel = { id: "", label: "" };
let withoutModel = { id: "", label: "" };
let categorised = { id: "", name: "" };

test.beforeAll(async ({ request }) => {
  classId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `subtitled_${stamp}`, name: CLASS })).id;
  const attr = (body: Record<string, unknown>) => apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, ...body });
  const hostname = (await attr({ key: "hostname", label: "Hostname", dataType: "text", sortOrder: 10 })).id;
  modelId = (await attr({ key: "model", label: "Model", dataType: "text", sortOrder: 20 })).id;
  await apiSend(request, "PATCH", `/ci-classes/${classId}`, { titleAttributeId: hostname });
  const ci = (attributes: Record<string, unknown>) => apiSend<{ id: string; label: string }>(request, "POST", "/configuration-items", { classId, attributes });
  withModel = await ci({ hostname: `sub-a-${stamp}`, model: "PowerEdge R760" });
  withoutModel = await ci({ hostname: `sub-b-${stamp}` });

  // Two types between CIs of the class: one gets a category in the UI below, the other stays without one.
  const type = async (key: string, name: string, forwardLabel: string, reverseLabel: string) => {
    const rt = await apiSend<{ id: string; name: string }>(request, "POST", "/relationship-types", { key, name, forwardLabel, reverseLabel, isDirectional: true, sortOrder: 9000 });
    await apiSend(request, "POST", "/relationship-rules", { relationshipTypeId: rt.id, sourceClassId: classId, targetClassId: classId });
    await apiSend(request, "POST", "/relationships", { relationshipTypeId: rt.id, sourceCiId: withModel.id, targetCiId: withoutModel.id });
    return rt;
  };
  categorised = await type(`patched_to_${stamp}`, `Patched to ${stamp}`, "is patched to", "is patched from");
  await type(`mirrors_${stamp}`, `Mirrors ${stamp}`, "mirrors", "is mirrored by");
});

test("G8: the subtitle field is set on the type page and shows under the CI's name", async ({ page, request }) => {
  await page.goto(`/admin/classes/${classId}`);
  const subtitle = page.getByLabel("Subtitle field");
  await expect(subtitle.locator("option")).toHaveText(["None – show the type name", "Hostname (hostname)", "Model (model)"]);
  await subtitle.selectOption({ label: "Model (model)" });
  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  expect((await apiGet<{ subtitleAttributeId: string | null }>(request, `/ci-classes/${classId}`)).subtitleAttributeId).toBe(modelId);
  await page.reload();
  await expect(subtitle.locator("option:checked")).toHaveText("Model (model)");

  // The inventory: the model under the name; the CI without one shows the class name (under its name, or in
  // the class column when the list has one).
  await page.goto(`/cis?classId=${classId}`);
  const row = (id: string) => page.locator(`tr[data-id='${id}']`);
  await expect(row(withModel.id).locator(".ci-name-cell .ci-subtitle")).toHaveText("PowerEdge R760");
  await expect(row(withoutModel.id)).toContainText(CLASS);
  await expect(row(withoutModel.id)).not.toContainText("PowerEdge R760");

  // The CI page head: the model under the name; without a value only the class chip follows the name.
  await page.goto(`/cis/${withModel.id}`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(withModel.label);
  await expect(page.getByTestId("record-subtitle")).toHaveText("PowerEdge R760");
  await page.goto(`/cis/${withoutModel.id}`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(withoutModel.label);
  await expect(page.getByTestId("record-subtitle")).toHaveCount(0);
  await expect(page.getByTestId("record-meta").getByRole("link", { name: CLASS, exact: true })).toBeVisible();

  // "None" clears it again.
  await page.goto(`/admin/classes/${classId}`);
  await subtitle.selectOption({ label: "None – show the type name" });
  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  expect((await apiGet<{ subtitleAttributeId: string | null }>(request, `/ci-classes/${classId}`)).subtitleAttributeId).toBeNull();
});

test("G15: a relationship type's category is set in its dialog and groups the CI's relationships", async ({ page, request }) => {
  await page.goto(`/admin/relationships?type=${categorised.id}`);
  await page.getByRole("button", { name: `Actions for ${categorised.name}` }).click();
  await page.getByRole("menuitem", { name: "Edit" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Category").fill(CATEGORY);
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect((await apiGet<{ category: string | null }>(request, `/relationship-types/${categorised.id}`)).category).toBe(CATEGORY);
  await page.reload();
  await expect(page.locator("table.relationship-types tr").filter({ hasText: categorised.name })).toContainText(CATEGORY);
  await page.getByRole("button", { name: `Actions for ${categorised.name}` }).click();
  await page.getByRole("menuitem", { name: "Edit" }).click();
  await expect(page.getByRole("dialog").getByLabel("Category")).toHaveValue(CATEGORY);
  await page.keyboard.press("Escape");

  // The CI page: the categorised type under its heading, the other under "Other", after it.
  await page.goto(`/cis/${withModel.id}`);
  const rel = page.getByRole("region", { name: /^Relationships/ });
  await expect(rel.locator(".rel-group-head")).toHaveText([`${CATEGORY} 1`, "Other 1"]);
  const grouped = rel.getByRole("list", { name: new RegExp(`^${CATEGORY}`) }).getByRole("listitem");
  await expect(grouped.getByRole("link")).toHaveText(withoutModel.label);
  await expect(grouped.locator(".rel-type")).toHaveText("is patched to");
  await expect(rel.getByRole("list", { name: /^Other/ }).locator(".rel-type")).toHaveText("mirrors");
});

test("G16: the CI page head says who changed the CI last, and only the time without audit.view", async ({ page, browser, request }) => {
  await page.goto(`/cis/${withModel.id}`);
  await expect(page.getByTestId("record-meta").locator("time")).toHaveText(/^Updated .+ by e2e-admin$/);

  // A user who may view the class but not the audit log: the time, no name, no error.
  const profileId = (
    await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: `E2E no audit ${stamp}`,
      globalPermissions: [],
      classPermissions: [{ classId, view: true, create: false, edit: false, delete: false }],
    })
  ).id;
  await apiSend(request, "POST", "/admin/users", { username: USERNAME, email: `${USERNAME}@example.test`, displayName: `No audit ${stamp}`, password: PASSWORD, profileIds: [profileId] });
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const other = await context.newPage();
  await other.goto("/login");
  await other.getByLabel("Username").fill(USERNAME);
  await other.getByLabel("Password").fill(PASSWORD);
  await other.getByRole("button", { name: "Sign in" }).click();
  await expect(other.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await other.goto(`/cis/${withModel.id}`);
  await expect(other.getByRole("heading", { level: 1 })).toHaveText(withModel.label);
  const time = other.getByTestId("record-meta").locator("time");
  await expect(time).toHaveText(/^Updated .+$/);
  await expect(time).not.toContainText(" by ");
  await expect(other.getByRole("alert")).toHaveCount(0);
  await context.close();
});
