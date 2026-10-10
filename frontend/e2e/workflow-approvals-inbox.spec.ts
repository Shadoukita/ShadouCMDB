import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, expect, openUserMenu, test } from "./support";

// The approvals inbox and delegations (SHAA-2916, design SHAA-1869 A6b). Two approvers share the first step: A,
// who may view the CI type, and R, who may not. The navigation count and the inbox of each match what they can
// open (A: one request, R: none). A decides from the inbox. The administrator delegates B's approvals on the
// admin page; the delegate finds it under My delegations, delegates their own approvals by exact username (they
// may not look up users), and revokes the delegation they received.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "approvals-inbox-password-123";
const REQUESTER = `e2e-ai-req-${stamp}`;
const APPROVER_A = `e2e-ai-a-${stamp}`;
const APPROVER_B = `e2e-ai-b-${stamp}`;
const RESTRICTED = `e2e-ai-r-${stamp}`;
const DELEGATE = `e2e-ai-d-${stamp}`;
const WF = `E2E inbox change ${stamp}`;
const WF_KEY = `e2e_ai_${stamp}`;
const CI = `ai-ci-${stamp}`;

let ciId = "";

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

const approvalsLink = (page: Page) => page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Approvals/ });

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Ai ${stamp}`, key: `ai_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  const profileName = `E2E ai operators ${stamp}`;
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: profileName,
    globalPermissions: [],
    classPermissions: [{ classId: cls.id, view: true, create: true, edit: true, delete: false }],
  });
  // R holds no right on the type: an approver by name, who must never see the request.
  const none = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E ai none ${stamp}`, globalPermissions: [], classPermissions: [] });
  for (const u of [REQUESTER, APPROVER_A, APPROVER_B, DELEGATE, RESTRICTED]) {
    const profileIds = [u === RESTRICTED ? none.id : profile.id];
    await apiSend(request, "POST", "/admin/users", { username: u, email: `${u}@example.test`, displayName: u, password: PASSWORD, profileIds });
  }

  const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: WF, key: WF_KEY, classId: cls.id });
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${def.id}/draft`, {
    initialState: "draft",
    states: [
      { key: "draft", name: "Draft", category: "open" },
      { key: "done", name: "Done", category: "done", terminal: true },
    ],
    transitions: [
      {
        key: "submit",
        name: "Submit",
        from: "draft",
        to: "done",
        approval: {
          steps: [
            { key: "tech", name: "Technical review", requiredApprovals: 1, dueAfter: "P2D" },
            { key: "cab", name: "CAB", requiredApprovals: 1 },
          ],
        },
      },
    ],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${def.id}/draft/publish`, { expectedDraftChecksum: draft.checksum });
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${def.id}`, { version: cur.version, isActive: true });
  const grants = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}/grants`);
  await apiSend(request, "PUT", `/admin/workflow-definitions/${def.id}/grants`, { version: grants.version, grants: [{ transitionKey: "submit", profiles: [profileName] }] });
  const approvers = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}/approvers`);
  await apiSend(request, "PUT", `/admin/workflow-definitions/${def.id}/approvers`, {
    version: approvers.version,
    approvers: [
      { transitionKey: "submit", stepKey: "tech", source: "user", user: APPROVER_A },
      { transitionKey: "submit", stepKey: "tech", source: "user", user: RESTRICTED },
      { transitionKey: "submit", stepKey: "cab", source: "user", user: APPROVER_B },
    ],
  });
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls.id, attributes: { name: CI } })).id;
  await apiSend(request, "POST", "/workflow-instances", { definitionKey: WF_KEY, ciId });
});

test("the requester's request is listed under Requested by me", async ({ browser }) => {
  const page = await signInUi(browser, REQUESTER);
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: "Workflows" }).click();
  await page.getByTestId("ci-workflows").getByTestId("wf-transition-submit").click();
  await page.getByRole("dialog", { name: `Submit: ${WF}` }).getByRole("button", { name: "Request approval" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Approval requested" })).toBeVisible();

  await approvalsLink(page).click();
  await expect(page.getByTestId("approvals-empty")).toContainText("Nothing waits for your decision");
  await page.getByTestId("approvals-view-requested").click();
  await expect(page).toHaveURL(/\/approvals\?view=requested$/);
  const table = page.getByTestId("approvals-table");
  await expect(table).toContainText(CI);
  await expect(table).toContainText("Pending");
  await page.reload();
  await expect(page.getByTestId("approvals-view-requested")).toHaveAttribute("aria-current", "page");
  await page.context().close();
});

test("a restricted approver's count matches what they can open: none", async ({ browser }) => {
  const page = await signInUi(browser, RESTRICTED);
  await expect(approvalsLink(page).locator(".nav-pending")).toHaveCount(0);
  await approvalsLink(page).click();
  await expect(page.getByTestId("approvals-empty")).toContainText("Nothing waits for your decision");
  await page.context().close();
});

test("approver A sees one in the count and the inbox, and approves from there", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, APPROVER_A);
  await expect(approvalsLink(page).locator(".nav-pending")).toContainText("1");
  await approvalsLink(page).click();
  await expect(page.getByTestId("approvals-view-actionable").locator(".count")).toHaveText("1");
  const row = page.getByTestId("approvals-table").getByRole("row").filter({ hasText: CI });
  await expect(row).toContainText("Step 1 of 2 (Technical review): 0 of 1");
  await checkA11y(page, testInfo, "approvals inbox", { include: "main" });
  await row.getByRole("button", { name: /Review request/ }).click();
  const dlg = page.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await dlg.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: /Your approval of Technical review is recorded/ })).toBeVisible();
  await expect(page.getByTestId("approvals-empty")).toBeVisible();
  await expect(approvalsLink(page).locator(".nav-pending")).toHaveCount(0);
  await page.getByTestId("approvals-view-decided").click();
  await expect(page.getByTestId("approvals-table")).toContainText(CI);
  await page.context().close();
});

test("the administrator delegates B's approvals on the admin page", async ({ page }, testInfo) => {
  await page.goto("/admin/approval-delegations");
  await expect(page.getByRole("heading", { level: 1, name: "Approval delegations" })).toBeVisible();
  await page.getByTestId("delegation-new").click();
  const dlg = page.getByRole("dialog", { name: "Delegate a user's approvals" });
  // Nothing picked: both problems are named before anything is sent.
  await dlg.getByRole("button", { name: "Delegate", exact: true }).click();
  await expect(dlg.getByTestId("delegation-principal")).toContainText("Choose whose approvals to delegate.");
  await dlg.getByRole("combobox", { name: "Whose approvals" }).fill(APPROVER_B);
  await dlg.getByRole("option", { name: new RegExp(APPROVER_B) }).click();
  await dlg.getByRole("combobox", { name: "Delegate" }).fill(DELEGATE);
  await dlg.getByRole("option", { name: new RegExp(DELEGATE) }).click();
  await dlg.getByLabel("Reason").fill("e2e: annual leave");
  await checkA11y(page, testInfo, "delegation dialog", { include: "dialog[open]" });
  await dlg.getByRole("button", { name: "Delegate", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `${DELEGATE} may now decide approvals for ${APPROVER_B}` })).toBeVisible();
  await expect(page.getByRole("table", { name: "Approval delegations" }).getByRole("row").filter({ hasText: DELEGATE })).toContainText("Active");
});

test("the delegate finds it under My delegations, delegates by exact username, and revokes it", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, DELEGATE);
  await openUserMenu(page);
  await page.getByRole("link", { name: "My delegations" }).click();
  await expect(page).toHaveURL(/\/account\/delegations$/);
  const row = page.getByRole("table", { name: "My delegations" }).getByRole("row").filter({ hasText: APPROVER_B });
  await expect(row).toContainText(`${DELEGATE} (you)`);
  await expect(row).toContainText("All workflows");
  await expect(row).toContainText("e2e: annual leave");
  await checkA11y(page, testInfo, "my delegations", { include: "main" });

  // Without business-service edit or users.manage, the first answer of the candidate lookup is `exactMatchOnly`:
  // the picker becomes a username field, looked up on Enter or blur. A miss does not say why.
  await page.getByTestId("delegation-new").click();
  const dlg = page.getByRole("dialog", { name: "Delegate my approvals" });
  await dlg.getByRole("combobox", { name: "Delegate" }).fill(APPROVER_A.slice(0, 8));
  const username = dlg.getByRole("textbox", { name: "Delegate's username" });
  await expect(username).toHaveValue(APPROVER_A.slice(0, 8));
  await expect(username).toBeFocused();
  await expect(dlg.getByTestId("delegation-delegate")).toContainText("Enter the delegate's exact username.");
  await username.fill(`nobody-${stamp}`);
  await username.press("Enter");
  await expect(dlg.getByTestId("delegation-delegate")).toContainText("No active user with this username to whom you can delegate.");
  await username.fill(APPROVER_A.toUpperCase());
  await username.press("Enter");
  await expect(dlg.getByTestId("delegation-delegate")).toContainText(`Delegate: ${APPROVER_A} (${APPROVER_A})`);
  await checkA11y(page, testInfo, "delegation dialog, exact username", { include: "dialog[open]" });
  await dlg.getByRole("button", { name: "Delegate", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `${APPROVER_A} may now decide approvals for ${DELEGATE}` })).toBeVisible();
  await expect(page.getByRole("table", { name: "My delegations" }).getByRole("row").filter({ hasText: APPROVER_A })).toContainText("Active");

  await row.getByRole("button", { name: `Revoke the delegation of ${APPROVER_B} to ${DELEGATE}` }).click();
  await page.getByRole("dialog", { name: "Revoke this delegation?" }).getByRole("button", { name: "Revoke" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Revoked: ${DELEGATE} no longer decides for ${APPROVER_B}.` })).toBeVisible();
  await expect(row).toContainText("Revoked");
  await expect(row.getByRole("button", { name: /Revoke/ })).toHaveCount(0);
  await page.context().close();
});

