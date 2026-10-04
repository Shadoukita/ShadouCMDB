import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, expect, test } from "./support";

// Running workflows on CIs (SHAA-1428, v0.4.0 S6): the CI's Workflows tab with the transition dialog, the
// instance list with its per-state summary, an instance's history, and the CI history. As an operator who may
// view and edit one type only and is granted one transition of two: a CI of another type and its instance
// never show, and the transition they are not granted is not offered. The administrator runs the rest.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "wf-runtime-password-123";
const OPERATOR = `e2e-wf-operator-${stamp}`;
const WF = `E2E change ${stamp}`;
const WF_KEY = `e2e_change_${stamp}`;
const HIDDEN_WF = `E2E hidden ${stamp}`;
const CI = `wf-ci-${stamp}`;
const HIDDEN_CI = `wf-hidden-ci-${stamp}`;

let ciId = "";
let hiddenInstanceId = "";
let instanceId = "";

/** A workflow on `classId`: Draft → (Submit: needs Environment = prod, a comment) → Review → (Approve) → Done. */
async function workflow(request: Parameters<typeof apiSend>[0], classId: string, name: string, key: string, profileName: string) {
  const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name, key, classId });
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${def.id}/draft`, {
    initialState: "draft",
    states: [
      { key: "draft", name: "Draft", category: "open" },
      { key: "review", name: "Review", category: "active" },
      { key: "done", name: "Done", category: "done", terminal: true },
    ],
    transitions: [
      {
        key: "submit",
        name: "Submit",
        from: "draft",
        to: "review",
        requiresComment: true,
        fields: [{ attribute: "env", required: true }],
        conditions: { field: "env", op: "eq", value: "prod" },
      },
      { key: "approve", name: "Approve", from: "review", to: "done" },
    ],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${def.id}/draft/publish`, { expectedDraftChecksum: draft.checksum });
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${def.id}`, { version: cur.version, isActive: true });
  const grants = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}/grants`);
  // The operator's profile may submit, not approve.
  await apiSend(request, "PUT", `/admin/workflow-definitions/${def.id}/grants`, { version: grants.version, grants: [{ transitionKey: "submit", profiles: [profileName] }] });
  return def.id;
}

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

const workflowsTab = (page: Page) => page.getByRole("tab", { name: "Workflows" });
const panel = (page: Page) => page.getByTestId("ci-workflows");

test.beforeAll(async ({ request }) => {
  const cls = async (name: string, key: string) => {
    const { id } = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name, key });
    const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: id, key: "name", label: "Name", dataType: "text" });
    await apiSend(request, "PATCH", `/ci-classes/${id}`, { titleAttributeId: title.id });
    await apiSend(request, "POST", "/attribute-definitions", { classId: id, key: "env", label: "Environment", dataType: "enum", enumValues: ["test", "prod"] });
    return id;
  };
  const publicId = await cls(`Wf public ${stamp}`, `wf_public_${stamp}`);
  const hiddenId = await cls(`Wf hidden ${stamp}`, `wf_hidden_${stamp}`);
  const profileName = `E2E wf operators ${stamp}`;
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: profileName,
    globalPermissions: [],
    classPermissions: [{ classId: publicId, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: OPERATOR, email: `${OPERATOR}@example.test`, displayName: OPERATOR, password: PASSWORD, profileIds: [profile.id] });

  await workflow(request, publicId, WF, WF_KEY, profileName);
  await workflow(request, hiddenId, HIDDEN_WF, `e2e_hidden_${stamp}`, profileName);
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: publicId, attributes: { name: CI, env: "test" } })).id;
  const hiddenCi = await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: hiddenId, attributes: { name: HIDDEN_CI, env: "prod" } });
  hiddenInstanceId = (
    await apiSend<{ instance: { id: string } }>(request, "POST", "/workflow-instances", { definitionKey: `e2e_hidden_${stamp}`, ciId: hiddenCi.id })
  ).instance.id;
});

test("an operator starts a workflow, sees why a transition is blocked and runs it with its field and comment", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, OPERATOR);
  await page.goto(`/cis/${ciId}`);
  await workflowsTab(page).click();
  await expect(panel(page)).toContainText("No workflow has run on this CI");

  await panel(page).getByRole("button", { name: "Start workflow" }).first().click();
  const start = page.getByRole("dialog", { name: "Start a workflow" });
  // Only the workflow of this CI's type is offered.
  await expect(start.getByLabel("Workflow")).toContainText(WF);
  await expect(start.getByLabel("Workflow")).not.toContainText(HIDDEN_WF);
  await start.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Started ${WF} on ${CI}.` })).toBeVisible();

  const row = panel(page).getByRole("row").filter({ hasText: WF });
  await expect(row).toContainText("Draft");
  await expect(row).toContainText("Running");
  // Submit is offered though its condition fails (Environment is test); Approve is not granted, and from Draft not possible.
  const submit = row.getByTestId("wf-transition-submit");
  await expect(submit).toHaveClass(/wf-blocked/);
  await expect(row.getByTestId("wf-transition-approve")).toHaveCount(0);
  await expect(row.getByRole("button", { name: "Cancel workflow" })).toHaveCount(0);

  await submit.click();
  const dlg = page.getByRole("dialog", { name: `Submit: ${WF}` });
  await expect(dlg.getByTestId("wf-blocked")).toBeVisible();
  await expect(dlg.getByLabel("Environment")).toHaveValue("test");
  await checkA11y(page, testInfo, "transition dialog", { include: "dialog[open]" });

  // Without a comment and with the failing value: 422 WORKFLOW_CONDITION_FAILED, the comment's message next to it.
  await dlg.getByRole("button", { name: "Submit", exact: true }).click();
  await expect(dlg.getByRole("alert")).toContainText("conditions are not met");
  await expect(dlg.locator("#wf-comment")).toHaveAttribute("aria-invalid", "true");

  await dlg.getByLabel("Environment").selectOption("prod");
  await dlg.getByLabel("Comment").fill("Ready for review");
  await dlg.getByRole("button", { name: "Submit", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Submit: ${CI} is now Review.` })).toBeVisible();
  await expect(row).toContainText("Review");
  // No grant for Approve: nothing to run.
  await expect(row).toContainText("No transition you may run");
  await page.context().close();
});

test("the operator's instance list and summary leave out the CI type they may not view", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, OPERATOR);
  await page.getByRole("link", { name: "Workflows" }).first().click();
  await expect(page).toHaveURL(/\/workflows$/);
  await expect(page.getByTestId("wf-summary")).toContainText(WF);
  await expect(page.getByTestId("wf-summary")).not.toContainText(`e2e_hidden_${stamp}`);
  await expect(page.locator("table.data").last()).toContainText(CI);
  await expect(page.locator("table.data").last()).not.toContainText(HIDDEN_CI);

  // A count filters the list, in the URL.
  await page.getByTestId("wf-summary").getByRole("button", { name: /Review/ }).first().click();
  await expect(page).toHaveURL(new RegExp(`workflow=${WF_KEY}.*state=review|state=review.*workflow=${WF_KEY}`));
  await page.reload();
  await expect(page.getByLabel("State")).toHaveValue("review");
  await expect(page.getByRole("link", { name: CI })).toBeVisible();

  // The instance and its history.
  await page.getByRole("link", { name: WF }).first().click();
  await expect(page).toHaveURL(/\/workflows\/[0-9a-f-]{36}$/);
  instanceId = new URL(page.url()).pathname.split("/").pop()!;
  const events = page.getByTestId("wf-events");
  await expect(events).toContainText("Started");
  await expect(events).toContainText("Submit");
  await expect(events).toContainText("Ready for review");
  await expect(events).toContainText("Environment");
  await expect(page.getByRole("button", { name: "Force state…" })).toHaveCount(0);
  await checkA11y(page, testInfo, "workflow instance", { include: "main" });

  // An instance on a CI of a type they may not view does not exist for them.
  await page.goto(`/workflows/${hiddenInstanceId}`);
  await expect(page.getByRole("heading", { name: "Workflow instance not found" })).toBeVisible();
  await page.context().close();
});

test("a version conflict asks to reload; the administrator then approves and the CI history shows the steps", async ({ page, request }) => {
  await page.goto(`/cis/${ciId}`);
  await workflowsTab(page).click();
  const row = panel(page).getByRole("row").filter({ hasText: WF });
  await row.getByTestId("wf-transition-approve").click();
  const dlg = page.getByRole("dialog", { name: `Approve: ${WF}` });
  await expect(dlg).toBeVisible();

  // Someone else moves the instance on in between.
  const inst = await apiGet<{ instance: { version: number } }>(request, `/workflow-instances/${instanceId}`);
  await apiSend(request, "POST", `/workflow-instances/${instanceId}/force`, { expectedVersion: inst.instance.version, stateKey: "draft", reason: "e2e: back to draft" });
  const again = await apiGet<{ instance: { version: number } }>(request, `/workflow-instances/${instanceId}`);
  await apiSend(request, "POST", `/workflow-instances/${instanceId}/force`, { expectedVersion: again.instance.version, stateKey: "review", reason: "e2e: back to review" });

  await dlg.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(dlg.getByTestId("wf-conflict")).toBeVisible();
  await dlg.getByRole("button", { name: "Reload the workflow" }).click();
  await expect(dlg).toBeHidden();

  await row.getByTestId("wf-transition-approve").click();
  await page.getByRole("dialog", { name: `Approve: ${WF}` }).getByRole("button", { name: "Approve", exact: true }).click();
  await expect(row).toContainText("Done");
  await expect(row).toContainText("Completed");

  await page.getByRole("tab", { name: "History" }).click();
  const steps = page.getByTestId("history-workflow");
  await expect(steps.filter({ hasText: "approve" })).toBeVisible();
  await expect(steps.filter({ hasText: "Ready for review" })).toBeVisible();
  await expect(steps.filter({ hasText: "e2e: back to draft" })).toBeVisible();
});
