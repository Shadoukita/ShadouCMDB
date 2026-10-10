import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, csrf, expect, test } from "./support";

// The Lifecycle template in Administration › Workflows › New (SHAA-3040, roadmap v0.4.0 "Lifecycle workflows"):
// one step creates Planned (initial) → Active → Retired (final) with Planned → Retired, its states mapped to the
// state field's values and its transitions granted to a profile, as an ordinary draft that the lint passes
// without a problem. Published, it moves a CI Planned → Active → Retired from the CI's Workflows tab, and it goes
// through the configuration export and import (format 8). A failed step deletes the half-built workflow again,
// and a manager scoped to one type is offered only that type.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const CLASS = `Wf lifecycle ${stamp}`;
const NAME = `E2E lifecycle ${stamp}`;
const KEY = `e2e_lifecycle_${stamp}`;
const PROFILE = `E2E lifecycle operators ${stamp}`;
const CI = `wf-lifecycle-ci-${stamp}`;
const PASSWORD = "wf-lifecycle-password-123";
const MANAGER = `e2e-wf-lifecycle-mgr-${stamp}`;

let classId = "";
let wfId = "";
let ciId = "";
const values: Record<string, string> = {};

interface Draft {
  initialState: string | null;
  states: { key: string; name: string; category: string; terminal?: boolean; stateValue?: string | null }[];
  transitions: { key: string; from: string; to: string }[];
}

test.beforeAll(async ({ request }) => {
  classId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: CLASS, key: `wf_lifecycle_${stamp}` })).id;
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${classId}`, { titleAttributeId: title.id });
  // A status list of its own, keyed like the IT infrastructure starter's: Active maps to in_service.
  const list = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `wf_lifecycle_status_${stamp}`, name: `Lifecycle status ${stamp}` });
  for (const [key, name] of [
    ["planned", "Planned"],
    ["in_service", "In service"],
    ["retired", "Retired"],
  ]) {
    values[key] = (await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key, name })).id;
  }
  await apiSend(request, "POST", "/attribute-definitions", { classId, key: "lifecycle", label: "Lifecycle status", dataType: "lookup", lookupListId: list.id });
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: PROFILE,
    globalPermissions: [],
    classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
  });
  // A workflow manager who may view and edit this type only (GH#667).
  const managers = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E lifecycle managers ${stamp}`,
    globalPermissions: ["workflows.manage"],
    classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", {
    username: MANAGER,
    email: `${MANAGER}@example.test`,
    displayName: MANAGER,
    password: PASSWORD,
    profileIds: [profile.id, managers.id],
  });
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, attributes: { name: CI, lifecycle: values.planned } })).id;
});

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

/** Fills in the new-workflow form for the lifecycle on this test's type, with its status field. */
async function fillLifecycle(page: Page, name: string) {
  await page.getByLabel("Name").fill(name);
  await page.getByLabel("CI type").selectOption({ label: CLASS });
  await page.getByLabel("State field", { exact: true }).selectOption({ label: "Lifecycle status (lifecycle)" });
}

test("New lifecycle workflow: the template's states, their values and the grants, created in one step", async ({ page, request }, testInfo) => {
  await page.goto("/admin/workflows");
  await page.getByTestId("wf-new-lifecycle").click();
  await expect(page).toHaveURL(/\/admin\/workflows\/new\?template=lifecycle$/);
  const choice = page.getByTestId("wf-template-choice");
  await expect(choice.getByLabel(/^Lifecycle/)).toBeChecked();
  // Blank and back: the choice is in the URL.
  await choice.getByLabel(/^Blank workflow/).check();
  await expect(page).toHaveURL(/\/admin\/workflows\/new$/);
  await expect(page.getByTestId("wf-template-panel")).toHaveCount(0);
  await choice.getByLabel(/^Lifecycle/).check();
  await expect(page).toHaveURL(/template=lifecycle/);

  await fillLifecycle(page, NAME);
  await expect(page.getByLabel("Key")).toHaveValue(KEY);
  const panel = page.getByTestId("wf-template-panel");
  const states = panel.getByRole("table", { name: "States the template creates" });
  await expect(states.getByRole("row").filter({ hasText: "Planned" })).toContainText("Initial");
  await expect(states.getByRole("row").filter({ hasText: "Active" })).toContainText("In service (in_service)");
  await expect(states.getByRole("row").filter({ hasText: "Retired" })).toContainText("Final");
  await expect(panel).toContainText("Put into service: Planned → Active");
  await expect(panel).toContainText("Cancel plan: Planned → Retired");
  await panel.getByLabel(PROFILE).check();
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);
  await checkA11y(page, testInfo, "workflow-new-lifecycle", { include: "main" });

  await page.getByRole("button", { name: "Create workflow" }).click();
  await expect(page).toHaveURL(/\/admin\/workflows\/[0-9a-f-]{36}\?tab=designer$/);
  wfId = new URL(page.url()).pathname.split("/").pop()!;
  await expect(page.getByRole("status").filter({ hasText: `Workflow ${NAME} created. Check the lifecycle draft in the designer` })).toBeVisible();

  // An ordinary draft: the designer shows it, and the lint finds no problem at all (the transitions are granted).
  await expect(page.getByTestId("wf-save-state")).toHaveText("All changes saved");
  await expect(page.getByRole("heading", { name: "States (3)" })).toBeVisible();
  await expect(page.getByTestId("wf-lint")).toContainText("No problems: the draft can be published.");
  await expect(page.getByTestId("wf-lint").locator(".badge.ok")).toHaveText("Ready to publish");

  const draft = await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`);
  expect(draft.initialState).toBe("planned");
  expect(draft.states.map((s) => [s.key, s.category, s.terminal ?? false, s.stateValue])).toEqual([
    ["planned", "open", false, "planned"],
    ["active", "active", false, "in_service"],
    ["retired", "done", true, "retired"],
  ]);
  expect(draft.transitions.map((x) => [x.key, x.from, x.to])).toEqual([
    ["activate", "planned", "active"],
    ["retire", "active", "retired"],
    ["cancel_plan", "planned", "retired"],
  ]);
  const grants = await apiGet<{ grants: { transitionKey: string; profiles: { name: string }[] }[] }>(request, `/admin/workflow-definitions/${wfId}/grants`);
  expect(grants.grants.map((g) => [g.transitionKey, g.profiles.map((p) => p.name)]).sort()).toEqual([
    ["activate", [PROFILE]],
    ["cancel_plan", [PROFILE]],
    ["retire", [PROFILE]],
  ]);
});

test("publish it and move a CI Planned → Active → Retired", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: "Publish version 1…" }).click();
  await page.getByRole("dialog", { name: "Publish version 1?" }).getByRole("button", { name: "Publish" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Version 1 published." })).toBeVisible();
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, isActive: true });

  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: "Workflows" }).click();
  const panel = page.getByTestId("ci-workflows");
  await panel.getByRole("button", { name: "Start workflow" }).first().click();
  const start = page.getByRole("dialog", { name: "Start a workflow" });
  await start.getByLabel("Workflow").selectOption({ label: `${NAME} (v1)` });
  await start.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Started ${NAME} on ${CI}.` })).toBeVisible();
  const row = panel.getByRole("row").filter({ hasText: NAME });
  await expect(row).toContainText("Planned");

  await row.getByTestId("wf-transition-activate").click();
  let dlg = page.getByRole("dialog", { name: `Put into service: ${NAME}` });
  await dlg.getByRole("button", { name: "Put into service", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Put into service: ${CI} is now Active.` })).toBeVisible();
  await expect(row).toContainText("Active");

  await row.getByTestId("wf-transition-retire").click();
  dlg = page.getByRole("dialog", { name: `Retire: ${NAME}` });
  await dlg.getByRole("button", { name: "Retire", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Retire: ${CI} is now Retired.` })).toBeVisible();

  // The state field followed the states.
  const ci = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${ciId}`);
  expect(JSON.stringify(ci.attributes.lifecycle)).toContain(values.retired);
});

test("the published lifecycle goes through the configuration export and import (format 8)", async ({ request }) => {
  const res = await request.get("/api/v1/admin/config/export", { headers: { "X-CSRF-Token": await csrf(request) } });
  expect(res.ok(), `export → ${res.status()}`).toBeTruthy();
  const file = (await res.json()) as { format: string; formatVersion: number; workflows?: { key: string; name: string }[] };
  const exported = file.workflows?.find((w) => w.key === KEY);
  expect(exported, "the lifecycle is in the export").toBeTruthy();

  // The same workflow under a new key, inactive (the original already drives the field): the import creates it and
  // publishes its version 1.
  const copyKey = `${KEY}_copy`;
  const imported = await request.post("/api/v1/admin/config/import?mode=apply", {
    data: { format: file.format, formatVersion: file.formatVersion, workflows: [{ ...exported, key: copyKey, name: `${NAME} copy`, isActive: false }] },
    headers: { "X-CSRF-Token": await csrf(request) },
  });
  expect(imported.ok(), `import → ${imported.status()} ${await imported.text()}`).toBeTruthy();
  const list = await apiGet<{ data: { id: string; key: string; currentVersionNo: number | null }[] }>(request, `/admin/workflow-definitions?q=${copyKey}`);
  const copy = list.data.find((w) => w.key === copyKey)!;
  expect(copy.currentVersionNo).toBe(1);
  const v1 = await apiGet<Draft>(request, `/admin/workflow-definitions/${copy.id}/versions/1`);
  expect(v1.states.map((s) => [s.key, s.stateValue])).toEqual([
    ["planned", "planned"],
    ["active", "in_service"],
    ["retired", "retired"],
  ]);
});

test("a failed step deletes the half-built workflow again", async ({ page, request }) => {
  const name = `E2E lifecycle refused ${stamp}`;
  await page.route("**/api/v1/admin/workflow-definitions/*/draft", (route) =>
    route.request().method() === "PUT"
      ? route.fulfill({ status: 503, json: { error: { code: "SERVICE_UNAVAILABLE", message: "The service is unavailable", details: [] } } })
      : route.fallback(),
  );
  await page.goto("/admin/workflows/new?template=lifecycle");
  await fillLifecycle(page, name);
  await page.getByRole("button", { name: "Create workflow" }).click();
  await expect(page.getByRole("status").filter({ hasText: `so workflow ${name} was not kept` })).toBeVisible();
  await expect(page).toHaveURL(/\/admin\/workflows\/new\?template=lifecycle$/);
  await expect(page.getByTestId("wf-template-orphan")).toHaveCount(0);
  const left = await apiGet<{ data: { name: string }[] }>(request, `/admin/workflow-definitions?q=${encodeURIComponent(name)}`);
  expect(left.data).toHaveLength(0);
});

test("a manager scoped to one type is offered only that type (GH#667)", async ({ browser }) => {
  const page = await signInUi(browser, MANAGER);
  await page.goto("/admin/workflows");
  await page.getByTestId("wf-new-lifecycle").click();
  await expect(page.getByTestId("wf-template-panel")).toBeVisible();
  const type = page.getByLabel("CI type");
  await expect(type.locator("option", { hasText: CLASS })).toBeEnabled();
  await expect(type.locator("option:not([disabled])")).toHaveCount(1);
  // Listing profiles needs profiles.manage or users.manage: the grants are left to the Grants tab.
  await expect(page.getByTestId("wf-template-panel")).toContainText("Grant the transitions on the Grants tab");
  await page.context().close();
});
