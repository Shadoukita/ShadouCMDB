import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, csrf, expect, test } from "./support";

// The Change (RFC) template in Administration › Workflows › New (SHAA-3105, roadmap v0.4.0 "Change management
// template"): one step creates Requested (initial) → Assessment → Approved → Implementation → Review → Closed, with
// Rejected from the assessment or a cancelled request, and a CAB approval step on Approve (CAB). The lint then
// reports only that the CAB has no approvers; once one is assigned on the Approvers tab it is ready to publish.
// Published, it moves a CI through the whole change from the CI's Workflows tab, the CAB member approving in their
// own session, and the Assessment state links to the CI's Impact tab. It goes through the configuration export and
// import, a failed step deletes the half-built workflow again, and a manager scoped to one type is
// offered only that type (GH#667).
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const CLASS = `Wf change ${stamp}`;
const NAME = `E2E change ${stamp}`;
const KEY = `e2e_change_${stamp}`;
const OPERATORS = `E2E change operators ${stamp}`;
const CAB = `E2E change CAB ${stamp}`;
const CI = `wf-change-ci-${stamp}`;
const PASSWORD = "wf-change-password-123";
const CAB_MEMBER = `e2e-wf-change-cab-${stamp}`;
const MANAGER = `e2e-wf-change-mgr-${stamp}`;

let classId = "";
let wfId = "";
let ciId = "";

interface Draft {
  initialState: string | null;
  states: { key: string; category: string; terminal?: boolean }[];
  transitions: { key: string; from: string; to: string; requiresComment?: boolean; approval?: { steps: { key: string; requiredApprovals?: number; excludeActorsOf?: string[] }[] } | null }[];
}

test.beforeAll(async ({ request }) => {
  classId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: CLASS, key: `wf_change_${stamp}` })).id;
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${classId}`, { titleAttributeId: title.id });
  const view = [{ classId, view: true, create: false, edit: false, delete: false }];
  await apiSend(request, "POST", "/admin/profiles", { name: OPERATORS, globalPermissions: [], classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }] });
  // The CAB: a profile that may view the type, with one member.
  const cab = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: CAB, globalPermissions: [], classPermissions: view });
  await apiSend(request, "POST", "/admin/users", { username: CAB_MEMBER, email: `${CAB_MEMBER}@example.test`, displayName: CAB_MEMBER, password: PASSWORD, profileIds: [cab.id] });
  // A workflow manager who may view and edit this type only (GH#667).
  const managers = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E change managers ${stamp}`,
    globalPermissions: ["workflows.manage"],
    classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: MANAGER, email: `${MANAGER}@example.test`, displayName: MANAGER, password: PASSWORD, profileIds: [managers.id] });
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, attributes: { name: CI } })).id;
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

const panel = (page: Page) => page.getByTestId("ci-workflows");
const flash = (page: Page, text: string | RegExp) => page.getByRole("status").filter({ hasText: text });

async function openCi(page: Page) {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: "Workflows" }).click();
  await expect(panel(page).getByRole("row").filter({ hasText: NAME })).toBeVisible();
}

/** Runs a transition without approval from the CI's Workflows tab. */
async function run(page: Page, key: string, name: string, state: string) {
  await panel(page).getByTestId(`wf-transition-${key}`).click();
  await page.getByRole("dialog", { name: `${name}: ${NAME}` }).getByRole("button", { name, exact: true }).click();
  await expect(flash(page, `${name}: ${CI} is now ${state}.`)).toBeVisible();
}

test("New workflow › Change: the template's states, transitions and CAB step, created in one step", async ({ page, request }, testInfo) => {
  await page.goto("/admin/workflows/new");
  const choice = page.getByTestId("wf-template-choice");
  await choice.getByLabel(/^Change \(RFC/).check();
  await expect(page).toHaveURL(/template=change/);

  await page.getByLabel("Name").fill(NAME);
  await expect(page.getByLabel("Key")).toHaveValue(KEY);
  await page.getByLabel("CI type").selectOption({ label: CLASS });
  const tpl = page.getByTestId("wf-template-panel");
  const states = tpl.getByRole("table", { name: "States the template creates" });
  await expect(states.getByRole("row")).toHaveCount(8);
  await expect(states.getByRole("row").filter({ hasText: "Requested" })).toContainText("Initial");
  await expect(states.getByRole("row").filter({ hasText: "Closed" })).toContainText("Final");
  await expect(states.getByRole("row").filter({ hasText: "Rejected" })).toContainText("Final");
  await expect(tpl).toContainText("Approve (CAB): Assessment → Approved");
  await expect(tpl).toContainText("Approval: CAB approval, 1 of the assigned approvers; whoever ran Submit for assessment may not approve");
  await expect(tpl).toContainText("Cancel request: Requested → Rejected");
  await expect(tpl.getByTestId("wf-template-approvers")).toContainText("choose them on the Approvers tab");
  await tpl.getByLabel(OPERATORS).check();
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);
  await checkA11y(page, testInfo, "workflow-new-change", { include: "main" });

  await page.getByRole("button", { name: "Create workflow" }).click();
  await expect(page).toHaveURL(/\/admin\/workflows\/[0-9a-f-]{36}\?tab=designer$/);
  wfId = new URL(page.url()).pathname.split("/").pop()!;
  await expect(flash(page, `Workflow ${NAME} created. Choose the CAB approvers on the Approvers tab`)).toBeVisible();

  // An ordinary draft; the lint's only finding is the CAB without approvers.
  await expect(page.getByTestId("wf-save-state")).toHaveText("All changes saved");
  await expect(page.getByRole("heading", { name: "States (7)" })).toBeVisible();
  const lint = page.getByTestId("wf-lint");
  await expect(lint.locator("li")).toHaveCount(1);
  await expect(lint).toContainText("Nobody is assigned to approve step cab of transition approve");
  await expect(lint).toContainText("Choose them on the Approvers tab.");
  await expect(lint.locator(".badge.danger")).toHaveCount(0);

  const draft = await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`);
  expect(draft.initialState).toBe("requested");
  expect(draft.states.map((s) => [s.key, s.category, s.terminal ?? false])).toEqual([
    ["requested", "open", false],
    ["assessment", "active", false],
    ["approved", "active", false],
    ["implementation", "active", false],
    ["review", "active", false],
    ["closed", "done", true],
    ["rejected", "cancelled", true],
  ]);
  expect(draft.transitions.map((x) => [x.key, x.from, x.to])).toEqual([
    ["submit", "requested", "assessment"],
    ["approve", "assessment", "approved"],
    ["reject", "assessment", "rejected"],
    ["implement", "approved", "implementation"],
    ["complete", "implementation", "review"],
    ["close", "review", "closed"],
    ["cancel", "requested", "rejected"],
  ]);
  const cab = draft.transitions.find((x) => x.key === "approve")!.approval!.steps;
  expect(cab.map((s) => [s.key, s.requiredApprovals, s.excludeActorsOf])).toEqual([["cab", 1, ["submit"]]]);
  const grants = await apiGet<{ grants: { transitionKey: string; profiles: { name: string }[] }[] }>(request, `/admin/workflow-definitions/${wfId}/grants`);
  expect(grants.grants.filter((g) => g.profiles.some((p) => p.name === OPERATORS))).toHaveLength(7);
});

test("assign the CAB on the Approvers tab: the lint is clean, and the workflow is published", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  const step = page.getByTestId("wf-approvers-approve-cab");
  await expect(step.getByRole("heading")).toContainText("Approve (CAB) › CAB approval");
  await step.getByLabel("Source", { exact: true }).selectOption("profile");
  await step.getByLabel("Profile", { exact: true }).selectOption({ label: CAB });
  await step.getByRole("button", { name: "Add" }).click();
  await page.getByTestId("wf-approvers-save").click();
  await expect(flash(page, /Approvers saved/)).toBeVisible();

  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByTestId("wf-lint")).toContainText("No problems: the draft can be published.");
  await page.getByRole("button", { name: "Publish version 1…" }).click();
  await page.getByRole("dialog", { name: "Publish version 1?" }).getByRole("button", { name: "Publish" }).click();
  await expect(flash(page, "Version 1 published.")).toBeVisible();
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, isActive: true });
});

test("move a CI Requested → Assessment → (CAB approves) → Implementation → Review → Closed", async ({ page, request, browser }) => {
  await apiSend(request, "POST", "/workflow-instances", { definitionKey: KEY, ciId });
  await openCi(page);
  const row = panel(page).getByRole("row").filter({ hasText: NAME });
  await expect(row).toContainText("Requested");
  await expect(row.getByTestId("wf-impact-link")).toHaveCount(0);

  await run(page, "submit", "Submit for assessment", "Assessment");
  // Assessing the change: the impact analysis is one click away.
  await expect(row.getByTestId("wf-impact-link")).toHaveText("Impact analysis");
  await row.getByTestId("wf-impact-link").click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ciId}/impact$`));
  await expect(page.getByRole("tab", { name: "Impact" })).toHaveAttribute("aria-selected", "true");

  // The submitter asks the CAB.
  await openCi(page);
  await panel(page).getByTestId("wf-transition-approve").click();
  const ask = page.getByRole("dialog", { name: `Approve (CAB): ${NAME}` });
  await expect(ask.getByTestId("wf-requires-approval")).toContainText("This transition requires approval.");
  await ask.getByRole("button", { name: "Request approval" }).click();
  await expect(flash(page, `Approval requested: Approve (CAB) on ${CI}.`)).toBeVisible();

  // The CAB member decides in their own session.
  const member = await signInUi(browser, CAB_MEMBER);
  await openCi(member);
  await panel(member).getByTestId("wf-pending-approval").getByRole("button", { name: "Review and decide" }).click();
  await member.getByRole("dialog", { name: `Approval: Approve (CAB) on ${CI}` }).getByRole("button", { name: "Approve", exact: true }).click();
  await expect(flash(member, `Approved: Approve (CAB) ran on ${CI}, which is now Approved.`)).toBeVisible();
  await member.context().close();

  await openCi(page);
  await expect(panel(page).getByRole("row").filter({ hasText: NAME })).toContainText("Approved");
  await expect(panel(page).getByTestId("wf-impact-link")).toHaveCount(0);
  await run(page, "implement", "Start implementation", "Implementation");
  await run(page, "complete", "Implementation complete", "Review");
  await run(page, "close", "Close", "Closed");

  const list = await apiGet<{ data: { instance: { definitionKey?: string; definitionName: string; status: string; state: { key: string } } }[] }>(request, `/configuration-items/${ciId}/workflows`);
  const inst = list.data.find((v) => v.instance.definitionName === NAME)!.instance;
  expect([inst.state.key, inst.status]).toEqual(["closed", "completed"]);
});

test("the change workflow goes through the configuration export and import", async ({ request }) => {
  const res = await request.get("/api/v1/admin/config/export", { headers: { "X-CSRF-Token": await csrf(request) } });
  expect(res.ok(), `export → ${res.status()}`).toBeTruthy();
  const file = (await res.json()) as { format: string; formatVersion: number; workflows?: { key: string; name: string }[] };
  const exported = file.workflows?.find((w) => w.key === KEY);
  expect(exported, "the change workflow is in the export").toBeTruthy();

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
  expect(v1.states.map((s) => s.key)).toEqual(["requested", "assessment", "approved", "implementation", "review", "closed", "rejected"]);
  expect(v1.transitions.find((x) => x.key === "approve")?.approval?.steps.map((s) => s.key)).toEqual(["cab"]);
});

test("a failed step deletes the half-built change workflow again", async ({ page, request }) => {
  const name = `E2E change refused ${stamp}`;
  await page.route("**/api/v1/admin/workflow-definitions/*/draft", (route) =>
    route.request().method() === "PUT"
      ? route.fulfill({ status: 503, json: { error: { code: "SERVICE_UNAVAILABLE", message: "The service is unavailable", details: [] } } })
      : route.fallback(),
  );
  await page.goto("/admin/workflows/new?template=change");
  await page.getByLabel("Name").fill(name);
  await page.getByLabel("CI type").selectOption({ label: CLASS });
  await page.getByRole("button", { name: "Create workflow" }).click();
  await expect(flash(page, `so workflow ${name} was not kept`)).toBeVisible();
  await expect(page.getByTestId("wf-template-orphan")).toHaveCount(0);
  const left = await apiGet<{ data: { name: string }[] }>(request, `/admin/workflow-definitions?q=${encodeURIComponent(name)}`);
  expect(left.data).toHaveLength(0);
});

test("a manager scoped to one type is offered only that type (GH#667)", async ({ browser }) => {
  const page = await signInUi(browser, MANAGER);
  await page.goto("/admin/workflows/new?template=change");
  await expect(page.getByTestId("wf-template-choice").getByLabel(/^Change \(RFC/)).toBeChecked();
  await expect(page.getByTestId("wf-template-panel")).toBeVisible();
  const type = page.getByLabel("CI type");
  await expect(type.locator("option", { hasText: CLASS })).toBeEnabled();
  await expect(type.locator("option:not([disabled])")).toHaveCount(1);
  await page.context().close();
});
