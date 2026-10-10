import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, expect, test } from "./support";

// Run-time approvals (SHAA-2916, design SHAA-1869 A6a): a transition with a two-step approval policy, walked by
// a requester, two approvers and a delegate in the browser. The transition dialog says it needs approval and
// lists the steps; the requester sees the pending banner and no approve button; the first approver rejects
// (a reason is required), the requester asks again, the first approver approves step 1, and a delegate decides
// step 2 for the second approver, which runs the transition. The instance page lists both requests.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "approvals-runtime-password-123";
const REQUESTER = `e2e-ar-req-${stamp}`;
const APPROVER_A = `e2e-ar-a-${stamp}`;
const APPROVER_B = `e2e-ar-b-${stamp}`;
const DELEGATE = `e2e-ar-d-${stamp}`;
const WF = `E2E approved change ${stamp}`;
const WF_KEY = `e2e_ar_${stamp}`;
const CI = `ar-ci-${stamp}`;

let ciId = "";
let instanceId = "";

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
const banner = (page: Page) => panel(page).getByTestId("wf-pending-approval");
const flash = (page: Page, text: string | RegExp) => page.getByRole("status").filter({ hasText: text });

async function openCi(page: Page) {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: "Workflows" }).click();
  await expect(panel(page).getByRole("row").filter({ hasText: WF })).toBeVisible();
}

/** The requester runs Submit, which only requests it. */
async function requestSubmit(page: Page) {
  await openCi(page);
  await panel(page).getByTestId("wf-transition-submit").click();
  const dlg = page.getByRole("dialog", { name: `Submit: ${WF}` });
  await expect(dlg.getByTestId("wf-requires-approval")).toContainText("This transition requires approval.");
  await expect(dlg.getByRole("list", { name: "Approval steps" }).getByRole("listitem")).toHaveText(["Technical review: 1 approval", "CAB: 1 approval"]);
  await dlg.getByLabel("Environment").selectOption("prod");
  await dlg.getByRole("button", { name: "Request approval" }).click();
  await expect(flash(page, `Approval requested: Submit on ${CI}.`)).toBeVisible();
  await expect(banner(page)).toContainText("Step 1 of 2: 0 of 1 approvals");
}

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Ar ${stamp}`, key: `ar_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  await apiSend(request, "POST", "/attribute-definitions", { classId: cls.id, key: "env", label: "Environment", dataType: "enum", enumValues: ["test", "prod"] });
  const profileName = `E2E ar operators ${stamp}`;
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: profileName,
    globalPermissions: [],
    classPermissions: [{ classId: cls.id, view: true, create: true, edit: true, delete: false }],
  });
  const ids: Record<string, string> = {};
  for (const u of [REQUESTER, APPROVER_A, APPROVER_B, DELEGATE]) {
    ids[u] = (await apiSend<{ id: string }>(request, "POST", "/admin/users", { username: u, email: `${u}@example.test`, displayName: u, password: PASSWORD, profileIds: [profile.id] })).id;
  }

  // Draft → (Submit, Environment, approval: Technical review by A, then CAB by B) → Done.
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
        fields: [{ attribute: "env" }],
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
      { transitionKey: "submit", stepKey: "cab", source: "user", user: APPROVER_B },
    ],
  });
  // B is away: the delegate decides for them.
  const now = Date.now();
  await apiSend(request, "POST", "/admin/approval-delegations", {
    principalUserId: ids[APPROVER_B],
    delegateUserId: ids[DELEGATE],
    startsAt: new Date(now - 60_000).toISOString(),
    endsAt: new Date(now + 86_400_000).toISOString(),
    reason: "e2e: on leave",
  });

  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls.id, attributes: { name: CI, env: "test" } })).id;
  instanceId = (await apiSend<{ instance: { id: string } }>(request, "POST", "/workflow-instances", { definitionKey: WF_KEY, ciId })).instance.id;
});

test("the requester asks for approval and sees the pending request without an approve button", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, REQUESTER);
  await requestSubmit(page);
  const row = panel(page).getByRole("row").filter({ hasText: WF });
  // No transition runs while the request is pending.
  await expect(row.getByTestId("wf-transition-submit")).toHaveCount(0);
  await expect(row).toContainText("Awaiting approval");
  await expect(banner(page).getByTestId("wf-pending-withdraw")).toBeVisible();

  await banner(page).getByRole("button", { name: "View request" }).click();
  const dlg = page.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await expect(dlg.getByTestId("approval-cannot-decide")).toContainText("You made this request. Under the four-eyes rule someone else must decide it.");
  await expect(dlg.getByRole("button", { name: "Approve" })).toHaveCount(0);
  await expect(dlg.getByRole("table", { name: "Approval steps" })).toContainText("Technical review");
  await checkA11y(page, testInfo, "approval request (requester)", { include: "dialog[open]" });
  await dlg.getByRole("button", { name: "Close" }).click();
  await page.context().close();
});

test("approver A rejects with a reason; the requester asks again", async ({ browser }, testInfo) => {
  const a = await signInUi(browser, APPROVER_A);
  await openCi(a);
  await banner(a).getByRole("button", { name: "Review and decide" }).click();
  const dlg = a.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await expect(dlg.getByRole("group", { name: "Your decision on Technical review" })).toBeVisible();
  await checkA11y(a, testInfo, "approval decision", { include: "dialog[open]" });
  await dlg.getByLabel("Reject", { exact: true }).check();
  await dlg.getByRole("button", { name: "Reject request" }).click();
  await expect(dlg.locator("#approval-comment")).toHaveAttribute("aria-invalid", "true");
  await expect(dlg).toContainText("Give a reason for the rejection.");
  await dlg.getByLabel("Comment").fill("Outside the change window");
  await dlg.getByRole("button", { name: "Reject request" }).click();
  await expect(flash(a, `Rejected: Submit on ${CI}. The CI stays as it is.`)).toBeVisible();
  await expect(banner(a)).toHaveCount(0);
  await a.context().close();

  const req = await signInUi(browser, REQUESTER);
  await requestSubmit(req);
  await req.context().close();
});

test("approver A approves step 1; the delegate approves step 2 for approver B, which runs the transition", async ({ browser }) => {
  const a = await signInUi(browser, APPROVER_A);
  await openCi(a);
  await banner(a).getByRole("button", { name: "Review and decide" }).click();
  let dlg = a.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await dlg.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(flash(a, /Your approval of Technical review is recorded/)).toBeVisible();
  await expect(banner(a)).toContainText("Step 2 of 2: 0 of 1 approvals");
  // A approved step 1, so step 2 needs someone else.
  await banner(a).getByRole("button", { name: "View request" }).click();
  dlg = a.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await expect(dlg.getByTestId("approval-cannot-decide")).toContainText("You approved an earlier step of this request.");
  await a.context().close();

  const d = await signInUi(browser, DELEGATE);
  await openCi(d);
  await banner(d).getByRole("button", { name: "Review and decide" }).click();
  dlg = d.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await expect(dlg.getByLabel("Decide as")).toHaveValue(/.+/);
  await expect(dlg.getByLabel("Decide as").locator("option")).toHaveText([`For ${APPROVER_B} (delegation)`]);
  await expect(dlg).toContainText("Yours is the last approval needed: Submit runs now and the workflow moves to Done.");
  await dlg.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(flash(d, `Approved: Submit ran on ${CI}, which is now Done.`)).toBeVisible();
  await expect(banner(d)).toHaveCount(0);
  await expect(panel(d).getByRole("row").filter({ hasText: WF })).toContainText("Done");
  await d.context().close();
});

test("the instance page lists both requests, and the decision names whom the delegate decided for", async ({ page }, testInfo) => {
  await page.goto(`/workflows/${instanceId}`);
  const history = page.getByTestId("wf-approval-history");
  await expect(history).toContainText("2 requests, newest first");
  const rows = history.getByRole("row");
  await expect(rows.nth(1)).toContainText("Approved");
  await expect(rows.nth(2)).toContainText("Rejected");
  await checkA11y(page, testInfo, "approval request history", { include: "[data-testid=wf-approval-history]" });
  await rows.nth(1).getByRole("button", { name: /Open request/ }).click();
  const dlg = page.getByRole("dialog", { name: `Approval: Submit on ${CI}` });
  await expect(dlg.getByTestId("approval-status")).toHaveText("Approved");
  await expect(dlg.getByRole("table", { name: "Approval steps" })).toContainText(`by ${DELEGATE} for ${APPROVER_B}`);
});
