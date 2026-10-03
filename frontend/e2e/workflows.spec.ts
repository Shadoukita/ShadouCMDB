import type { Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, classIdByName, expect, test } from "./support";

// Administration › Workflows (SHAA-1426, v0.4.0 S5): create a workflow, design its draft with live lint,
// publish it with a change note, grant transitions to a profile, retire the version, activate it with
// its state field (the UNINSTANCED_CIS warning) and delete it. Runs on the demo seed: CI type Server,
// state field Status (lookup list `status`).
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const NAME = `E2E server lifecycle ${stamp}`;
const KEY = `e2e_server_lifecycle_${stamp}`;
const PROFILE = `E2E change managers ${stamp}`;
let wfId = "";

interface Draft {
  initialState: string | null;
  states: { key: string; name: string; category: string; terminal?: boolean; stateValue?: string | null }[];
  transitions: { key: string; name: string; from: string; to: string; requiresComment?: boolean; fields?: unknown[]; conditions?: unknown }[];
  layout: Record<string, { x: number; y: number }> | null;
  checksum: string;
}

const saveState = (page: Page) => page.getByTestId("wf-save-state");
const inspector = (page: Page) => page.locator(".wf-inspector");

/** Edits the selected state in the inspector: name, key (committed on change), category, state value. */
async function editState(page: Page, s: { name: string; key: string; category: string; value?: string; terminal?: boolean }) {
  const i = inspector(page);
  await i.getByLabel("Name").fill(s.name);
  await i.getByLabel("Key").fill(s.key);
  await i.getByLabel("Key").press("Tab");
  await i.getByLabel("Category").selectOption({ label: s.category });
  if (s.value) await i.getByLabel("Status value").selectOption({ label: s.value });
  if (s.terminal) await i.getByLabel("Terminal: reaching it completes the instance").check();
  await expect(page.getByRole("heading", { name: `State: ${s.name}` })).toBeVisible();
}

test("Workflows sits under Processes; an empty list says what a workflow is", async ({ page }) => {
  await page.goto("/admin/users");
  await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Workflows", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/workflows$/);
  await expect(page.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();

  await page.route("**/api/v1/admin/workflow-definitions?*", (route) => route.fulfill({ json: { data: [], page: { total: 0, limit: 50, offset: 0 } } }));
  await page.reload();
  await expect(page.getByTestId("workflows-empty")).toContainText("No workflows yet");
  await expect(page.getByTestId("workflows-empty").getByRole("link", { name: "New workflow" })).toBeVisible();
});

test("create a workflow for Server with Status as its state field", async ({ page, request }) => {
  await page.goto("/admin/workflows");
  await page.getByRole("link", { name: "+ New workflow" }).click();
  await expect(page).toHaveURL(/\/admin\/workflows\/new$/);
  // Required fields are flagged before anything is sent.
  await page.getByRole("button", { name: "Create workflow" }).click();
  await expect(page.locator("#wf-name")).toHaveAttribute("aria-invalid", "true");

  await page.getByLabel("Name").fill(NAME);
  await expect(page.getByLabel("Key")).toHaveValue(KEY);
  await page.getByLabel("CI type").selectOption({ label: "Server" });
  await page.getByLabel("State field", { exact: true }).selectOption({ label: "Status (status)" });
  await page.getByLabel("Description").fill("Plan, approve and run servers");
  await page.getByRole("button", { name: "Create workflow" }).click();

  await expect(page).toHaveURL(/\/admin\/workflows\/[0-9a-f-]{36}\?tab=designer$/);
  wfId = new URL(page.url()).pathname.split("/").pop()!;
  await expect(page.getByRole("status").filter({ hasText: `Workflow ${NAME} created.` })).toBeVisible();
  const wf = await apiGet<{ key: string; isActive: boolean; stateAttributeKey: string; draftVersionNo: number; classId: string }>(request, `/admin/workflow-definitions/${wfId}`);
  expect(wf).toMatchObject({ key: KEY, isActive: false, stateAttributeKey: "status", draftVersionNo: 1 });
  expect(wf.classId).toBe(await classIdByName(request, "Server"));
});

test("design the draft: states, transitions, a field and a condition, saved and linted live", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByRole("heading", { name: "No states yet" })).toBeVisible();

  // Three states. The first becomes the initial state.
  await page.getByRole("button", { name: "+ State" }).first().click();
  await editState(page, { name: "Planned", key: "planned", category: "Open", value: "Planned" });
  await page.getByRole("button", { name: "+ State" }).first().click();
  await editState(page, { name: "In service", key: "in_service", category: "In progress", value: "In service" });
  await page.getByRole("button", { name: "+ State" }).first().click();
  await editState(page, { name: "Retired", key: "retired", category: "Done", value: "Retired", terminal: true });

  // Saved and linted: Planned and In service have no way out yet, Retired cannot be reached.
  await expect(saveState(page)).toHaveText("All changes saved");
  const lint = page.getByTestId("wf-lint");
  await expect(lint.getByText(/errors?$/).first()).toBeVisible();
  await expect(lint).toContainText("State planned");
  await expect(page.getByRole("button", { name: /^Publish version 1/ })).toBeDisabled();

  // Planned → In service, with a field to fill in and a condition.
  await page.getByRole("button", { name: /^State Planned/ }).click();
  await page.getByRole("button", { name: "+ Transition" }).click();
  let i = inspector(page);
  await i.getByLabel("Name").fill("Put in service");
  await i.getByLabel("Key").fill("put_in_service");
  await i.getByLabel("Key").press("Tab");
  await i.getByLabel("To state").selectOption({ label: "In service" });
  await i.getByLabel("A comment is required to run it").check();
  await i.getByLabel("Field to add").selectOption({ label: "Environment" });
  await i.getByRole("button", { name: "Add", exact: true }).click();
  await i.getByRole("button", { name: "+ Condition" }).click();
  await i.getByLabel("Condition 1: field").selectOption({ label: "Environment" });
  await i.getByLabel("Condition 1: comparison").selectOption({ label: "is" });
  await i.getByLabel("Condition 1: value").selectOption({ label: "Production" });

  // In service → Retired.
  await page.getByRole("button", { name: /^State In service/ }).click();
  await page.getByRole("button", { name: "+ Transition" }).click();
  i = inspector(page);
  await i.getByLabel("Name").fill("Retire");
  await i.getByLabel("Key").fill("retire");
  await i.getByLabel("Key").press("Tab");
  await i.getByLabel("To state").selectOption({ label: "Retired" });

  // No errors left; the transitions no profile is granted are warnings, which do not stop publishing.
  await expect(saveState(page)).toHaveText("All changes saved");
  await expect(lint.getByText(/errors?$/)).toHaveCount(0);
  await expect(lint).toContainText("warning");
  await expect(page.getByRole("button", { name: "Publish version 1…" })).toBeEnabled();
  await checkA11y(page, testInfo, "workflow designer", { include: "main" });

  const draft = await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`);
  expect(draft.initialState).toBe("planned");
  expect(draft.states.map((s) => [s.key, s.category, s.stateValue, s.terminal ?? false])).toEqual([
    ["planned", "open", "planned", false],
    ["in_service", "active", "in_service", false],
    ["retired", "done", "retired", true],
  ]);
  expect(draft.transitions[0]).toMatchObject({
    key: "put_in_service",
    from: "planned",
    to: "in_service",
    requiresComment: true,
    fields: [{ attribute: "environment", required: true }],
    conditions: { all: [{ field: "environment", op: "eq", value: "production" }] },
  });
  expect(Object.keys(draft.layout ?? {}).sort()).toEqual(["in_service", "planned", "retired"]);

  // Moving a state with the keyboard saves the layout too.
  const before = draft.layout!.planned;
  await page.getByRole("button", { name: /^State Planned/ }).focus();
  await page.keyboard.press("Shift+ArrowDown");
  await expect(saveState(page)).toHaveText("All changes saved");
  await expect
    .poll(async () => (await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`)).layout!.planned.y)
    .toBe(before.y + 40);
});

test("a lint error lands on the state it is about", async ({ page }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: /^Transition Retire/ }).click();
  await inspector(page).getByRole("button", { name: "Delete transition" }).click();
  await expect(saveState(page)).toHaveText("All changes saved");
  // Retired is unreachable now, and In service a dead end.
  const row = page.getByRole("row").filter({ has: page.getByRole("button", { name: "Retired", exact: true }) });
  await expect(row.locator(".badge.danger")).toHaveText("Error");
  await page.getByRole("button", { name: "Retired", exact: true }).click();
  await expect(inspector(page).locator(".wf-problems")).toContainText("Error");
  await expect(page.getByRole("button", { name: /^Publish version 1/ })).toBeDisabled();

  // Put it back.
  await page.getByRole("button", { name: /^State In service/ }).click();
  await page.getByRole("button", { name: "+ Transition" }).click();
  const i = inspector(page);
  await i.getByLabel("Name").fill("Retire");
  await i.getByLabel("Key").fill("retire");
  await i.getByLabel("Key").press("Tab");
  await i.getByLabel("To state").selectOption({ label: "Retired" });
  await expect(saveState(page)).toHaveText("All changes saved");
  await expect(page.getByRole("button", { name: "Publish version 1…" })).toBeEnabled();
});

test("publish version 1 with a change note", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByRole("button", { name: "Publish version 1…" })).toBeEnabled();
  await page.getByRole("button", { name: "Publish version 1…" }).click();
  const dialog = page.getByRole("dialog", { name: "Publish version 1?" });
  await expect(dialog).toContainText("warning");
  await dialog.getByLabel("Change note").fill("First cut of the server lifecycle");
  await dialog.getByRole("button", { name: "Publish" }).click();

  await expect(page).toHaveURL(/tab=versions/);
  await expect(page.getByRole("status").filter({ hasText: "Version 1 published." })).toBeVisible();
  const row = page.getByRole("row").filter({ hasText: "First cut of the server lifecycle" });
  await expect(row).toContainText("Published");
  await expect(row).toContainText("Current");

  const versions = await apiGet<{ data: { versionNo: number; status: string; isCurrent: boolean; changeNote: string | null }[] }>(
    request,
    `/admin/workflow-definitions/${wfId}/versions`,
  );
  expect(versions.data).toEqual([expect.objectContaining({ versionNo: 1, status: "published", isCurrent: true, changeNote: "First cut of the server lifecycle" })]);

  // The read-only view of the version.
  await row.getByRole("button", { name: "View" }).click();
  await expect(page.getByRole("heading", { name: "Version 1 (read-only)" })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Transition Put in service/ })).toBeVisible();

  // With everything published, the designer offers a new draft from the current version.
  await page.getByRole("tab", { name: "Designer" }).click();
  await expect(page.getByTestId("wf-no-draft")).toContainText("Version 1 is current.");
});

test("grant transitions to a permission profile", async ({ page, request }) => {
  await apiSend(request, "POST", "/admin/profiles", { name: PROFILE, description: "Run the server lifecycle", globalPermissions: [], classPermissions: [] });
  await page.goto(`/admin/workflows/${wfId}?tab=grants`);
  const matrix = page.getByTestId("wf-grants");
  await expect(matrix.getByRole("rowheader", { name: /Put in service/ })).toBeVisible();
  await expect(matrix.getByRole("rowheader", { name: /Cancel an instance/ })).toBeVisible();
  await matrix.getByRole("checkbox", { name: `${PROFILE} may run Put in service` }).check();
  await matrix.getByRole("checkbox", { name: `${PROFILE} may run Retire` }).check();
  await page.getByRole("button", { name: "Save grants" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Grants saved." })).toBeVisible();

  const grants = await apiGet<{ grants: { transitionKey: string; profiles: { name: string }[] }[] }>(request, `/admin/workflow-definitions/${wfId}/grants`);
  const byKey = Object.fromEntries(grants.grants.map((g) => [g.transitionKey, g.profiles.map((p) => p.name)]));
  expect(byKey.put_in_service).toEqual([PROFILE]);
  expect(byKey.retire).toEqual([PROFILE]);
});

test("activating with a state field is confirmed and reports the CIs without an instance", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}`);
  await page.getByLabel("Active: new instances can start").check();
  await page.getByRole("button", { name: "Save changes" }).click();
  const dialog = page.getByRole("dialog", { name: "Activate a workflow that drives a state field?" });
  await expect(dialog).toContainText("bootstrap");
  await dialog.getByRole("button", { name: "Activate" }).click();
  // The demo seed has servers, none with an instance yet.
  await expect(page.getByTestId("wf-uninstanced")).toContainText("CIs without an instance");
  await expect(page.getByTestId("wf-uninstanced")).toContainText("bootstrap");
  expect((await apiGet<{ isActive: boolean }>(request, `/admin/workflow-definitions/${wfId}`)).isActive).toBe(true);

  // Deactivate again, so the Status field of servers stays editable for the other specs.
  await page.getByLabel("Active: new instances can start").uncheck();
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${NAME}.` })).toBeVisible();
  expect((await apiGet<{ isActive: boolean }>(request, `/admin/workflow-definitions/${wfId}`)).isActive).toBe(false);
});

test("retire version 1, then delete the workflow", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=versions`);
  await page.getByRole("button", { name: "Retire…" }).click();
  const dialog = page.getByRole("dialog", { name: "Retire version 1?" });
  await expect(dialog).toContainText("No published version is left");
  await dialog.getByRole("button", { name: "Retire version" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Version 1 retired." })).toBeVisible();
  await expect(page.getByRole("row").filter({ hasText: "First cut" })).toContainText("Retired");
  expect((await apiGet<{ currentVersionNo: number | null }>(request, `/admin/workflow-definitions/${wfId}`)).currentVersionNo).toBeNull();

  // The list shows it under its type, and filters by type in the URL.
  await page.goto("/admin/workflows");
  await page.getByLabel("CI type").selectOption({ label: "Server" });
  await expect(page).toHaveURL(/class=server/);
  await page.locator("#wf-q").fill(stamp);
  await expect(page.getByRole("link", { name: NAME })).toBeVisible();

  await page.getByRole("link", { name: NAME }).click();
  await page.getByRole("button", { name: "Delete workflow" }).click();
  await page.getByRole("dialog", { name: `Delete workflow ${NAME}?` }).getByRole("button", { name: "Delete workflow" }).click();
  await expect(page).toHaveURL(/\/admin\/workflows$/);
  await expect(page.getByRole("status").filter({ hasText: `Workflow ${NAME} deleted.` })).toBeVisible();
  const gone = await request.get(`/api/v1/admin/workflow-definitions/${wfId}`);
  expect(gone.status()).toBe(404);
});
