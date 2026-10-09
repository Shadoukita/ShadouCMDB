import type { Browser, Locator, Page } from "@playwright/test";
import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { apiGet, apiSend, checkA11y, expect, test } from "./support";

// Approvals in the workflow designer (SHAA-2798, design SHAA-1869 §13 A5), Rin's plan: create a 2-step policy on a
// transition, assign its approvers (a profile, a group, a user as escalation, a Person field of the CI and the
// business service owners), see the lint and the preview for one CI, publish (with the warning about instances on
// the older version), then export the configuration and import it again with the policy and approvers intact
// (format 9). A user without workflows.manage gets no editing controls. Builds its own type, users and group.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "approvals-password-123";
const CAB = `E2E CAB ${stamp}`;
const PROFILE = `E2E change viewers ${stamp}`;
const HEAD = `e2e-head-${stamp}`;
const MEMBERS = [`e2e-cab-a-${stamp}`, `e2e-cab-b-${stamp}`];
const OUTSIDER = `e2e-no-designer-${stamp}`;
let classId = "";
let wfId = "";
let ciId = "";
const CI_NAME = `E2E change ${stamp}`;

/** A screenshot of one region for the review (E2E_SCREENSHOT_DIR): the page scrolls inside main, so full-page shots cut it. */
async function shot(region: Locator, name: string) {
  const dir = process.env.E2E_SCREENSHOT_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  await region.screenshot({ path: join(dir, `${name}.png`) });
}

const saveState = (page: Page) => page.getByTestId("wf-save-state");
const inspector = (page: Page) => page.locator(".wf-inspector");
const approval = (page: Page) => inspector(page).getByTestId("wf-approval");
const step = (page: Page, j: number) => approval(page).getByTestId(`wf-step-${j}`);

test.beforeAll(async ({ request }) => {
  // A type with a Person field (the CI's owner): the field source resolves to the user linked to that Person.
  const classes = await apiGet<{ data: { id: string; systemRole: string | null }[] }>(request, "/ci-classes?limit=200");
  const personClass = classes.data.find((c) => c.systemRole === "person")!;
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Change ${stamp}`, key: `change_${stamp}` });
  classId = cls.id;
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${classId}`, { titleAttributeId: title.id });
  await apiSend(request, "POST", "/attribute-definitions", { classId, key: "change_owner", label: "Change owner", dataType: "reference", referenceClassId: personClass.id });
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, attributes: { name: CI_NAME } })).id;

  // CAB members may view the type; the escalation user too. The outsider has the audit log only.
  const viewers = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: PROFILE,
    globalPermissions: [],
    classPermissions: [{ classId, view: true, create: false, edit: false, delete: false }],
  });
  const outsiders = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E outsiders ${stamp}`, globalPermissions: ["audit.view"], classPermissions: [] });
  const user = (username: string, profileId: string) =>
    apiSend<{ id: string }>(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName: username, password: PASSWORD, profileIds: [profileId] });
  const ids = [];
  for (const u of MEMBERS) ids.push((await user(u, viewers.id)).id);
  await user(HEAD, viewers.id);
  await user(OUTSIDER, outsiders.id);
  const group = await apiSend<{ id: string; version: number }>(request, "POST", "/admin/groups", { name: CAB });
  await apiSend(request, "PUT", `/admin/groups/${group.id}/members`, { version: group.version, userIds: ids });

  // Version 1 without approval, active, with one running instance: publishing the policy must warn about it.
  const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: `E2E change approval ${stamp}`, key: `e2e_change_approval_${stamp}`, classId });
  wfId = def.id;
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, {
    initialState: "planned",
    states: [
      { key: "planned", name: "Planned", category: "open" },
      { key: "approved", name: "Approved", category: "active" },
      { key: "done", name: "Done", category: "done", terminal: true },
    ],
    transitions: [
      { key: "approve", name: "Approve", from: "planned", to: "approved" },
      { key: "implement", name: "Implement", from: "approved", to: "done" },
    ],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${wfId}/draft/publish`, { expectedDraftChecksum: draft.checksum });
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, isActive: true });
  await apiSend(request, "POST", "/workflow-instances", { definitionId: wfId, ciId });
});

test("the steps editor: a 2-step policy on Approve, with N, due periods and flags, saved and linted live", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: "Start a draft from version 1" }).click();
  await expect(saveState(page)).toHaveText("All changes saved");
  await page.locator("table").getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Transition: Approve" })).toBeVisible();
  await expect(approval(page)).toContainText("No approval: the transition runs as soon as it is started.");

  // Step 1: Technical review, 1 approval, due in 2 days.
  await approval(page).getByRole("button", { name: "Require approval" }).click();
  await step(page, 0).getByLabel("Step name").fill("Technical review");
  await step(page, 0).getByLabel("Step key").fill("tech");
  await step(page, 0).getByLabel("Due after: amount").fill("2");
  await step(page, 0).getByLabel("Due after: amount").press("Tab");
  await expect(step(page, 0).getByLabel("Due after: unit")).toHaveValue("days");

  // Step 2: CAB, 2 approvals, due in 5 days, whoever implemented may not approve. Keyboard only.
  await approval(page).getByRole("button", { name: "+ Step" }).focus();
  await page.keyboard.press("Enter");
  await step(page, 1).getByLabel("Step name").fill("CAB");
  await step(page, 1).getByLabel("Step key").fill("cab");
  await step(page, 1).getByLabel("Approvals required").fill("2");
  await step(page, 1).getByLabel("Due after: amount").fill("5");
  await step(page, 1).getByLabel("Due after: amount").press("Tab");
  await step(page, 1).getByLabel("When overdue").selectOption("flag");
  await step(page, 1).getByRole("checkbox", { name: /Implement/ }).check();

  // A local check before anything is sent: rejecting when overdue needs a due period.
  await step(page, 0).getByLabel("Due after: amount").fill("");
  await step(page, 0).getByLabel("Due after: amount").press("Tab");
  await expect(step(page, 0).getByLabel("When overdue")).toBeDisabled();
  await step(page, 0).getByLabel("Due after: amount").fill("2");
  await step(page, 0).getByLabel("Due after: amount").press("Tab");
  await step(page, 0).getByLabel("Approvals required").fill("0");
  await expect(step(page, 0).locator(".error")).toHaveText("Step 1: between 1 and 20 approvals.");
  await expect(saveState(page)).toHaveText("Not saved: fix the marked problems");
  await step(page, 0).getByLabel("Approvals required").fill("1");

  await expect(saveState(page)).toHaveText("All changes saved");
  const stored = await apiGet<{ transitions: { key: string; approval?: { steps: unknown[] } | null }[] }>(request, `/admin/workflow-definitions/${wfId}/draft`);
  expect(stored.transitions.find((t) => t.key === "approve")?.approval?.steps).toEqual([
    { key: "tech", name: "Technical review", requiredApprovals: 1, dueAfter: "P2D", onOverdue: "flag", distinctFromEarlier: true, excludeActorsOf: [], allowApiTokens: false },
    { key: "cab", name: "CAB", requiredApprovals: 2, dueAfter: "P5D", onOverdue: "flag", distinctFromEarlier: true, excludeActorsOf: ["implement"], allowApiTokens: false },
  ]);

  // The lint (from the API) warns that nobody may approve the steps yet; the transitions table shows the policy.
  await expect(page.getByTestId("wf-lint")).toContainText("warning");
  await expect(step(page, 0).locator(".wf-problems")).toBeVisible();
  await expect(page.getByTestId("wf-gated")).toHaveText("Approval, 2 steps: Technical review (1) → CAB (2)");
  await checkA11y(page, testInfo, "workflow approval steps", { include: ".wf-inspector" });
  await shot(inspector(page), "approvals-steps-en");
});

test("the approvers matrix: five sources and an escalation, with the lint per step", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  const tech = page.getByTestId("wf-approvers-approve-tech");
  const cab = page.getByTestId("wf-approvers-approve-cab");
  await expect(tech.getByRole("heading")).toContainText("Approve › Technical review");
  await expect(cab.getByRole("heading")).toContainText("needs 2 approvals");
  await expect(tech).toContainText("Nobody may approve this step yet.");

  // Technical review: the profile, and the Person in the CI's Change owner field.
  await tech.getByLabel("Source", { exact: true }).selectOption("profile");
  await tech.getByLabel("Profile", { exact: true }).selectOption({ label: PROFILE });
  await tech.getByRole("button", { name: "Add" }).click();
  await tech.getByLabel("Source", { exact: true }).selectOption("ci_attribute");
  await tech.getByLabel("Field", { exact: true }).selectOption({ label: "Change owner (change_owner)" });
  await tech.getByRole("button", { name: "Add" }).click();

  // CAB: the group (picked from the directory), business service owners, and an escalation user.
  await cab.getByLabel("Source", { exact: true }).selectOption("group");
  await cab.getByRole("combobox", { name: "Group" }).fill(CAB.slice(0, 12));
  await cab.getByRole("option", { name: new RegExp(CAB) }).click();
  await cab.getByRole("button", { name: "Add" }).click();
  await cab.getByLabel("Source", { exact: true }).selectOption("service_owner");
  await cab.getByLabel("Owner role").selectOption("business");
  await cab.getByRole("button", { name: "Add" }).click();
  await cab.getByLabel("Role", { exact: true }).selectOption("escalation");
  await cab.getByLabel("Source", { exact: true }).selectOption("user");
  await cab.getByRole("combobox", { name: "User" }).fill(HEAD);
  await cab.getByRole("option", { name: new RegExp(HEAD) }).click();
  await cab.getByRole("button", { name: "Add" }).click();
  // The same assignment twice is refused before it is sent.
  await cab.getByLabel("Role", { exact: true }).selectOption("approver");
  await cab.getByLabel("Source", { exact: true }).selectOption("service_owner");
  await expect(cab.getByText("The step already has this approver.")).toBeVisible();

  await expect(page.getByTestId("wf-approvers").locator(".panel-header .badge")).toHaveText("Unsaved changes");
  await page.getByTestId("wf-approvers-save").click();
  await expect(page.getByRole("status").filter({ hasText: /Approvers saved/ })).toBeVisible();
  await expect(tech.locator("tbody tr")).toHaveCount(2);
  await expect(cab.locator("tbody tr")).toHaveCount(3);
  await expect(cab).toContainText("Escalation (once overdue)");

  const stored = await apiGet<{ approvers: { stepKey: string; role: string; source: string }[] }>(request, `/admin/workflow-definitions/${wfId}/approvers`);
  expect(stored.approvers.map((a) => `${a.stepKey}:${a.role}:${a.source}`).sort()).toEqual(
    ["cab:approver:group", "cab:approver:service_owner", "cab:escalation:user", "tech:approver:ci_attribute", "tech:approver:profile"].sort(),
  );
  await checkA11y(page, testInfo, "workflow approvers", { include: "[data-testid=wf-approvers]" });
  await shot(page.getByTestId("wf-approvers"), "approvals-approvers-en");
});

test("a stale version gives a readable conflict, and reloading shows the stored approvers", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  const tech = page.getByTestId("wf-approvers-approve-tech");
  await expect(tech.locator("tbody tr")).toHaveCount(2);
  // Someone else changes the workflow in between.
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, description: `changed ${stamp}` });
  await tech.getByRole("button", { name: /^Remove / }).first().click();
  await page.getByTestId("wf-approvers-save").click();
  await expect(page.getByRole("alert")).toContainText("Someone changed this workflow");
  await page.getByRole("button", { name: "Load the current approvers" }).click();
  await expect(tech.locator("tbody tr")).toHaveCount(2);
});

test("the preview for one CI: who would be asked, resolved by the API", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  const preview = page.getByTestId("wf-approver-preview");
  await preview.getByLabel("Step", { exact: true }).selectOption({ label: "Approve › CAB" });
  await preview.locator("#wf-preview-ci").fill(CI_NAME);
  await page.getByRole("option", { name: new RegExp(CI_NAME) }).first().click();
  const run = page.waitForResponse((r) => r.url().includes(`/admin/workflow-definitions/${wfId}/approvers/preview`) && r.url().includes(`ciId=${ciId}`));
  await preview.getByTestId("wf-preview-run").click();
  expect((await run).status()).toBe(200);
  const result = preview.getByTestId("wf-preview-result");
  await expect(result).toContainText("2 users may decide this step");
  await expect(result).toContainText("the step needs 2 approvals");
  const users = preview.getByTestId("wf-preview-users");
  for (const m of MEMBERS) await expect(users.getByRole("row", { name: new RegExp(m) })).toContainText("Eligible");
  await expect(users.getByRole("row", { name: new RegExp(HEAD) })).toContainText("Only once overdue");
  await checkA11y(page, testInfo, "workflow approver preview", { include: "[data-testid=wf-approver-preview]" });
  await shot(preview, "approvals-preview-en");
});

test("publishing warns about the instance still running on version 1", async ({ page, request }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(saveState(page)).toHaveText("All changes saved");
  await page.getByRole("button", { name: /^Publish version 2/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByTestId("wf-publish-running")).toContainText("1 instance is running on an older version");
  await expect(dialog.getByTestId("wf-publish-running")).toContainText("Approve does not need this approval. Migrate it");
  await shot(dialog, "approvals-publish-en");
  await dialog.getByRole("button", { name: "Publish" }).click();
  await expect(page).toHaveURL(/tab=versions/);
  const v2 = await apiGet<{ transitions: { key: string; approval?: { steps: { key: string }[] } | null }[] }>(request, `/admin/workflow-definitions/${wfId}/versions/2`);
  expect(v2.transitions.find((t) => t.key === "approve")?.approval?.steps.map((s) => s.key)).toEqual(["tech", "cab"]);
});

test("export and import the configuration: the policy and the approvers survive (format 9)", async ({ page, request }) => {
  await page.goto("/admin/config");
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download configuration" }).click();
  const file = JSON.parse(readFileSync(await (await download).path(), "utf8")) as {
    formatVersion: number;
    workflows: { key: string; approvers: { transition: string; step: string; role: string }[] }[];
  };
  expect(file.formatVersion).toBeGreaterThanOrEqual(9);
  const exported = file.workflows.find((w) => w.key === `e2e_change_approval_${stamp}`)!;
  expect(exported.approvers).toHaveLength(5);

  // Drop the approvers, then import the file again: they come back as a whole.
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}/approvers`);
  await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/approvers`, { version: cur.version, approvers: [] });
  await page.reload();
  await page.locator("#config-file").setInputFiles({ name: "config.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(file)) });
  await page.getByRole("button", { name: "Apply import" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
  await expect(page.getByRole("status").filter({ hasText: /applied|Imported|import/i }).first()).toBeVisible();

  const back = await apiGet<{ approvers: unknown[] }>(request, `/admin/workflow-definitions/${wfId}/approvers`);
  expect(back.approvers).toHaveLength(5);
  const current = await apiGet<{ currentVersionNo: number }>(request, `/admin/workflow-definitions/${wfId}`);
  expect(current.currentVersionNo).toBe(2);
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  await expect(page.getByTestId("wf-approvers-approve-cab").locator("tbody tr")).toHaveCount(3);
});

async function signIn(browser: Browser, username: string, locale?: "de"): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] }, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  if (locale) await page.addInitScript((l) => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = l), locale);
  await page.goto("/login");
  await page.locator("#login-username").fill(username);
  await page.locator("#login-password").fill(PASSWORD);
  await page.locator("form button[type=submit]").click();
  await expect(page).not.toHaveURL(/\/login/);
  return page;
}

test("without workflows.manage there are no editing controls", async ({ browser }) => {
  const page = await signIn(browser, OUTSIDER);
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  await expect(page.getByTestId("permission-denied")).toBeVisible();
  await expect(page.getByTestId("wf-approvers")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Save approvers" })).toHaveCount(0);
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByTestId("wf-approval")).toHaveCount(0);
  await page.context().close();
});

test("screenshots in de", async ({ page }) => {
  test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  // A new draft from version 2 shows the published steps in the inspector.
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: /Start a draft from version 2/ }).click();
  await page.locator("table").getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.getByTestId("wf-approval")).toContainText("Genehmigung");
  await shot(inspector(page), "approvals-steps-de");
  await page.goto(`/admin/workflows/${wfId}?tab=approvers`);
  await expect(page.getByTestId("wf-approvers")).toContainText("Genehmigende");
  const preview = page.getByTestId("wf-approver-preview");
  await preview.getByLabel("Stufe").selectOption({ index: 1 });
  await preview.locator("#wf-preview-ci").fill(CI_NAME);
  await page.getByRole("option", { name: new RegExp(CI_NAME) }).first().click();
  await preview.getByTestId("wf-preview-run").click();
  await expect(preview.getByTestId("wf-preview-result")).toContainText("Benutzer dürfen über diese Stufe entscheiden");
  await shot(page.getByTestId("wf-approvers"), "approvals-approvers-de");
  await shot(preview, "approvals-preview-de");
});
