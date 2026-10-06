import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, expect, roValue, test } from "./support";

// Adopting an existing inventory (SHAA-1815, v0.4.0 S6 part 2): an administrator activates a workflow that drives a
// state field, follows the UNINSTANCED_CIS warning to the bootstrap, previews it (counts per state, values no state
// maps) and runs it. An operator who may edit the type then finds the state field locked on the CI form and on the
// detail page, while the other fields stay editable. Last (GH#645), a v2 that renames the open state is published and
// the running instances of v1 are migrated to it from the Versions tab, with a state map, a preview and a confirmation.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "wf-adopt-password-123";
const OPERATOR = `e2e-wf-adopt-${stamp}`;
const WF = `E2E adopt ${stamp}`;
const PLANNED = [`adopt-a-${stamp}`, `adopt-b-${stamp}`];
const LIMBO = `adopt-limbo-${stamp}`;
const NONE = `adopt-none-${stamp}`;

let wfId = "";
let classId = "";
let plannedId = "";

async function signInUi(browser: Browser, username: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  return page;
}

test.beforeAll(async ({ request }) => {
  classId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Wf adopt ${stamp}`, key: `wf_adopt_${stamp}` })).id;
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${classId}`, { titleAttributeId: title.id });
  await apiSend(request, "POST", "/attribute-definitions", { classId, key: "ticket", label: "Change ticket", dataType: "text" });
  const list = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `wf_adopt_phase_${stamp}`, name: `Adopt phase ${stamp}` });
  const values: Record<string, string> = {};
  for (const key of ["planned", "live", "limbo"]) {
    values[key] = (await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key, name: key === "limbo" ? "Limbo" : key })).id;
  }
  const phase = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", {
    classId,
    key: "phase",
    label: "Phase",
    dataType: "lookup",
    lookupListId: list.id,
  });

  // Planned → (Go live) → Live; "limbo" is a value no state maps.
  const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", {
    name: WF,
    key: `e2e_adopt_${stamp}`,
    classId,
    stateAttributeId: phase.id,
  });
  wfId = def.id;
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, {
    initialState: "planned",
    states: [
      { key: "planned", name: "Planned", category: "open", stateValue: "planned" },
      { key: "live", name: "Live", category: "done", terminal: true, stateValue: "live" },
    ],
    transitions: [{ key: "go_live", name: "Go live", from: "planned", to: "live" }],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${wfId}/draft/publish`, { expectedDraftChecksum: draft.checksum });

  // The inventory that existed before the workflow was active.
  const ci = (name: string, value?: string) =>
    apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, attributes: { name, ...(value ? { phase: values[value] } : {}) } });
  plannedId = (await ci(PLANNED[0], "planned")).id;
  await ci(PLANNED[1], "planned");
  await ci(LIMBO, "limbo");
  await ci(NONE);

  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E adopt operators ${stamp}`,
    globalPermissions: [],
    classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: OPERATOR, email: `${OPERATOR}@example.test`, displayName: OPERATOR, password: PASSWORD, profileIds: [profile.id] });
});

test("activate, follow the warning to the bootstrap, preview it and start the instances", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}`);
  const panel = page.getByTestId("wf-bootstrap");
  // Inactive: nothing to preview yet.
  await expect(panel).toContainText("Activate the workflow first");
  await expect(panel.getByRole("button", { name: "Preview bootstrap" })).toBeDisabled();

  await page.getByLabel("Active: new instances can start").check();
  await page.getByRole("button", { name: "Save changes" }).click();
  await page.getByRole("dialog", { name: "Activate a workflow that drives a state field?" }).getByRole("button", { name: "Activate" }).click();
  const warning = page.getByTestId("wf-uninstanced");
  await expect(warning).toContainText("4 CIs without an instance");
  await warning.getByRole("button", { name: "Adopt them with the bootstrap" }).click();
  await expect(panel).toBeFocused();

  // The dry run: two Planned CIs would start; Limbo and the CI without a value are skipped.
  await panel.getByRole("button", { name: "Preview bootstrap" }).click();
  const summary = panel.getByTestId("wf-bootstrap-summary");
  await expect(summary).toContainText("Would start");
  await expect(summary).toContainText("2 on version 1");
  await expect(panel.getByRole("table", { name: "CIs per state" }).getByRole("row").filter({ hasText: "Planned" })).toContainText("2");
  const unmapped = panel.getByTestId("wf-bootstrap-unmapped");
  await expect(unmapped).toContainText("2 CIs skipped");
  await expect(unmapped).toContainText("Limbo");
  await expect(unmapped).toContainText("No value");
  await checkA11y(page, testInfo, "bootstrap preview", { include: "[data-testid='wf-bootstrap']" });
  // A dry run writes nothing.
  const before = await apiGet<{ data: unknown[] }>(request, `/workflow-instances?definitionKey=e2e_adopt_${stamp}&status=active`);
  expect(before.data).toHaveLength(0);

  await panel.getByRole("button", { name: "Start 2 instances…" }).click();
  const dialog = page.getByRole("dialog", { name: `Start ${WF} on 2 CIs?` });
  await expect(dialog).toContainText("2 CIs are skipped");
  await dialog.getByRole("button", { name: "Start instances" }).click();
  await expect(panel.getByTestId("wf-bootstrap-done")).toContainText("Started 2 instances on version 1.");
  // The banner now counts the CIs the bootstrap skipped (Limbo and the one without a value), not the 4 from before.
  await expect(warning).toContainText("2 CIs without an instance");

  const after = await apiGet<{ data: { ciId: string }[] }>(request, `/workflow-instances?definitionKey=e2e_adopt_${stamp}&status=active`);
  expect(after.data).toHaveLength(2);
  // Run again: nothing is left to start.
  await panel.getByRole("button", { name: "Preview bootstrap" }).click();
  await expect(summary).toContainText("Already running");
  await expect(panel.getByRole("button", { name: "Nothing to start" })).toBeDisabled();
});

test("the operator finds the state field locked on the form and the detail page, the other fields editable", async ({ browser, request }) => {
  const workflows = await apiGet<{ controlledFields: string[] }>(request, `/configuration-items/${plannedId}/workflows`);
  expect(workflows.controlledFields).toEqual(["phase"]);

  const page = await signInUi(browser, OPERATOR);
  await page.goto(`/cis/${plannedId}/edit`);
  const phase = page.locator("[data-field='attributes.phase']");
  await expect(phase).toContainText("Set by a workflow");
  await expect(page.getByLabel("Phase")).toBeDisabled();
  await page.getByLabel("Change ticket").fill("CHG-1815");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${PLANNED[0]}.` })).toBeVisible();
  const ci = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${plannedId}`);
  expect(ci.attributes.ticket).toBe("CHG-1815");

  // The detail page edits fields in place: there the state field is shown as a value, with no input, and says why.
  await page.goto(`/cis/${plannedId}`);
  await expect(page.getByLabel("Change ticket")).toHaveValue("CHG-1815");
  await expect(page.getByLabel("Change ticket")).toBeEditable();
  const shown = page.locator(".field-ro[data-field='attributes.phase']");
  await expect(shown).toContainText("Set by a workflow");
  await expect(roValue(page, "attributes.phase")).toHaveText("planned");
  await expect(shown.locator("input, select, [role='combobox']")).toHaveCount(0);
  await page.context().close();
});

test("migrate the running instances of version 1 to a version 2 that renames their state", async ({ page, request }, testInfo) => {
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, {
    initialState: "queued",
    states: [
      { key: "queued", name: "Queued", category: "open", stateValue: "planned" },
      { key: "live", name: "Live", category: "done", terminal: true, stateValue: "live" },
    ],
    transitions: [{ key: "go_live", name: "Go live", from: "queued", to: "live" }],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${wfId}/draft/publish`, { expectedDraftChecksum: draft.checksum });

  await page.goto(`/admin/workflows/${wfId}?tab=versions`);
  const v1 = page.getByTestId("wf-version-1");
  await expect(v1).toContainText("2");
  await v1.getByRole("button", { name: "Migrate instances…" }).click();
  const panel = page.getByTestId("wf-migrate");
  await expect(panel.getByLabel("Target version")).toHaveValue("2");
  // v2 has no "planned": the operator must choose, and the preview says so next to the state.
  const map = panel.getByLabel("Target state for Planned");
  await expect(map).toHaveValue("");
  await panel.getByRole("button", { name: "Preview migration" }).click();
  await expect(panel.locator("#wf-migrate-map-planned-err")).toBeVisible();

  await map.selectOption("queued");
  await panel.getByRole("button", { name: "Preview migration" }).click();
  await expect(panel.getByTestId("wf-migrate-summary")).toContainText("2 to version 2");
  await expect(panel.getByRole("table", { name: "State map" }).getByRole("row").filter({ hasText: "Planned" })).toContainText("2");
  await checkA11y(page, testInfo, "migration preview", { include: "[data-testid='wf-migrate']" });
  // A dry run writes nothing.
  const before = await apiGet<{ data: { versionNo: number }[] }>(request, `/workflow-instances?definitionKey=e2e_adopt_${stamp}&status=active`);
  expect(before.data.map((i) => i.versionNo)).toEqual([1, 1]);

  await panel.getByRole("button", { name: "Migrate 2 instances…" }).click();
  const dialog = page.getByRole("dialog", { name: "Move 2 instances to version 2?" });
  await expect(dialog).toContainText("2 instances in planned → Queued");
  await dialog.getByRole("button", { name: "Migrate instances" }).click();
  await expect(panel.getByTestId("wf-migrate-done")).toContainText("Moved 2 instances from version 1 to version 2.");

  const after = await apiGet<{ data: { versionNo: number; state: { key: string } }[] }>(request, `/workflow-instances?definitionKey=e2e_adopt_${stamp}&status=active`);
  expect(after.data.map((i) => [i.versionNo, i.state.key])).toEqual([
    [2, "queued"],
    [2, "queued"],
  ]);
  // Nothing is left on v1 to migrate.
  await expect(v1.getByRole("button", { name: "Migrate instances…" })).toHaveCount(0);
});

test.afterAll(async ({ request }) => {
  // Leave the workflow inactive so its state field is editable again.
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, isActive: false });
});
