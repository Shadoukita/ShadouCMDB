import type { Browser, Locator, Page } from "@playwright/test";
import { de } from "../src/i18n/de";
import { en, type MessageKey } from "../src/i18n/en";
import { setLocaleForTests, t as translate, type Locale, type MessageParams } from "../src/i18n/index";
import { apiGet, apiSend, expect, expectDialogLaidOut, snap, test } from "./support";

// QA edges of the run-time approvals UI (#862) and the approvals inbox (#864), walked once in English and once with
// the test-only German locale forced. One workflow, one approval step "CAB" that needs 2 approvals from 3 approvers
// (A, B, P; the requester is listed too, so four-eyes, not the approver list, must keep them out). P delegated their
// approvals to D. Two CIs are requested:
//   CI 1: the requester asks in the UI, may not approve (no button, and the API refuses), A approves from the inbox
//         (1 of 2), D approves for P (2 of 2), which runs the transition.
//   CI 2: B rejects it from the CI page; a reason is required.
// After every decision the API is asked what was stored, and the counts (navigation badge, inbox tab) of every
// approver go down. Fixtures come from the API; the decisions are made in the browser.

const PASSWORD = "approvals-qa-edges-password-123";
const LOCALES: Locale[] = ["en", "de"];

/** The text of `key` in `locale`, as the app renders it (same catalog and formatter). */
function tr(locale: Locale, key: MessageKey, params: MessageParams = {}): string {
  setLocaleForTests(locale);
  try {
    return translate(key, params);
  } finally {
    setLocaleForTests(null);
  }
}
const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The approval slice's English texts that read differently in German: none of them may show in the German UI. */
const ENGLISH = (Object.keys(en) as MessageKey[])
  .filter((k) => /^(approvalRun|approvals)\./.test(k) && en[k] !== de[k] && !en[k].includes("{") && en[k].length >= 5)
  .map((k) => en[k]);
const asWords = (s: string) => new RegExp(`(?<![\\p{L}\\d])${esc(s)}(?![\\p{L}\\d])`, "u");

/** In German, no English text of the approvals UI may show in `scope`, neither as text nor as an accessible name. */
async function expectNoEnglish(locale: Locale, scope: Locator) {
  if (locale !== "de") return;
  const text = await scope.evaluate(
    (el) =>
      `${(el as HTMLElement).innerText}\n` +
      [...el.querySelectorAll("[aria-label], [title], [placeholder]")].flatMap((e) => ["aria-label", "title", "placeholder"].map((a) => e.getAttribute(a) ?? "")).join("\n"),
  );
  expect(ENGLISH.filter((s) => asWords(s).test(text)), "English approval texts in the German UI").toEqual([]);
}

/** No button or badge in `scope` cuts off its text (long German labels). */
async function expectNotClipped(scope: Locator) {
  const clipped = await scope.evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>(".btn, .badge, .count, .nav-pending, .nav-label")]
      .filter((el) => el.offsetParent !== null && el.scrollWidth > el.clientWidth + 1)
      .map((el) => el.textContent?.trim()),
  );
  expect(clipped, "clipped labels").toEqual([]);
}

interface ApprovalRequest {
  id: string;
  requestNo: number;
  status: string;
  version: number;
  steps: { key: string; approvals: number | string; requiredApprovals: number; decisions: { decision: string; actorName: string; onBehalfOfName: string | null; comment: string | null }[] }[];
}
interface Instance {
  id: string;
  version: number;
  state: { key: string };
  pendingApproval: { requestId: string; approvals: number | string; required: number } | null;
}

for (const locale of LOCALES) {
  test.describe(`approvals QA edges (${locale})`, () => {
    test.describe.configure({ mode: "serial" });

    const L = (key: MessageKey, params: MessageParams = {}) => tr(locale, key, params);
    const stamp = `${Date.now().toString(36)}${locale}`;
    const REQUESTER = `e2e-aq-req-${stamp}`;
    const APPROVER_A = `e2e-aq-a-${stamp}`;
    const APPROVER_B = `e2e-aq-b-${stamp}`;
    const PRINCIPAL = `e2e-aq-p-${stamp}`;
    const DELEGATE = `e2e-aq-d-${stamp}`;
    const WF = `E2E QA approvals ${stamp}`;
    const WF_KEY = `e2e_aq_${stamp}`;
    const CI1 = `aq-ci1-${stamp}`;
    const CI2 = `aq-ci2-${stamp}`;
    const ci = { 1: { id: "", instanceId: "", requestId: "", requestNo: 0 }, 2: { id: "", instanceId: "", requestId: "", requestNo: 0 } };

    /** A browser session of `username` in this locale, signed in through the API (the sign-in page is not under test). */
    async function session(browser: Browser, username: string): Promise<Page> {
      const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
      const res = await context.request.post("/api/v1/auth/login", { data: { username, password: PASSWORD } });
      expect(res.ok(), `sign in ${username}: ${res.status()}`).toBeTruthy();
      await context.addInitScript((l) => {
        (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = l;
      }, locale);
      const page = await context.newPage();
      const problems: string[] = [];
      page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
      page.on("console", (m) => {
        if (m.text().startsWith("Failed to load resource")) return;
        if (m.type() === "error" || m.text().includes("[Vue warn]")) problems.push(`${m.type()}: ${m.text()}`);
      });
      (page as Page & { problems?: string[] }).problems = problems;
      return page;
    }
    async function done(page: Page) {
      const problems = (page as Page & { problems?: string[] }).problems ?? [];
      await page.context().close();
      expect(problems, "page errors / Vue warnings").toEqual([]);
    }
    /** The API as the user of `page` (their session cookies and CSRF token). */
    async function asUser(page: Page, method: "GET" | "POST", path: string, data?: unknown) {
      const csrf = (await page.context().cookies()).find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
      return page.context().request.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": csrf } });
    }
    async function myCount(page: Page): Promise<number> {
      const res = await asUser(page, "GET", "/workflow-instances/counts");
      expect(res.ok()).toBeTruthy();
      return Number(((await res.json()) as { awaitingMyDecision: number }).awaitingMyDecision);
    }

    const navLink = (page: Page) => page.locator('a[href="/approvals"]:has(.nav-label)');
    const navBadge = (page: Page) => navLink(page).locator(".nav-pending");
    const tabCount = (page: Page) => page.getByTestId("approvals-view-actionable").locator(".count");
    const inboxRow = (page: Page, label: string) => page.getByTestId("approvals-table").getByRole("row").filter({ hasText: label });
    const panel = (page: Page) => page.getByTestId("ci-workflows");
    const banner = (page: Page) => panel(page).getByTestId("wf-pending-approval");
    const flash = (page: Page, text: string) => page.getByRole("status").filter({ hasText: text });
    const decisionDialog = (page: Page, label: string) => page.getByRole("dialog", { name: L("approvalRun.dialog.title", { transition: "Submit", ci: label }) });
    const progress = (n: number) => L("approvalRun.progress", { step: 1, steps: 1, n, required: 2 });

    async function openCi(page: Page, n: 1 | 2) {
      await page.goto(`/cis/${ci[n].id}`);
      await page.getByRole("tab", { name: L("record.tab.workflows") }).click();
      await expect(panel(page).getByRole("row").filter({ hasText: WF })).toBeVisible();
    }
    async function stored(n: 1 | 2) {
      return apiGet<ApprovalRequest>(adminRequest!, `/workflow-approval-requests/${ci[n].requestId}`);
    }

    // The administrator's API context of the running hook or test (the `request` fixture is per test).
    let adminRequest: Parameters<typeof apiGet>[0] | undefined;
    test.beforeEach(({ request }) => {
      adminRequest = request;
    });

    test.beforeAll(async ({ request, browser }) => {
      adminRequest = request;
      const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Aq ${stamp}`, key: `aq_${stamp}` });
      const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
      await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
      const profileName = `E2E aq operators ${stamp}`;
      const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
        name: profileName,
        globalPermissions: [],
        classPermissions: [{ classId: cls.id, view: true, create: true, edit: true, delete: false }],
      });
      const ids: Record<string, string> = {};
      for (const u of [REQUESTER, APPROVER_A, APPROVER_B, PRINCIPAL, DELEGATE]) {
        ids[u] = (await apiSend<{ id: string }>(request, "POST", "/admin/users", { username: u, email: `${u}@example.test`, displayName: u, password: PASSWORD, profileIds: [profile.id] })).id;
      }

      const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: WF, key: WF_KEY, classId: cls.id });
      const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${def.id}/draft`, {
        initialState: "draft",
        states: [
          { key: "draft", name: "Draft", category: "open" },
          { key: "done", name: "Done", category: "done", terminal: true },
        ],
        transitions: [{ key: "submit", name: "Submit", from: "draft", to: "done", approval: { steps: [{ key: "cab", name: "CAB", requiredApprovals: 2 }] } }],
      });
      await apiSend(request, "POST", `/admin/workflow-definitions/${def.id}/draft/publish`, { expectedDraftChecksum: draft.checksum });
      const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}`);
      await apiSend(request, "PATCH", `/admin/workflow-definitions/${def.id}`, { version: cur.version, isActive: true });
      const grants = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}/grants`);
      await apiSend(request, "PUT", `/admin/workflow-definitions/${def.id}/grants`, { version: grants.version, grants: [{ transitionKey: "submit", profiles: [profileName] }] });
      const approvers = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}/approvers`);
      await apiSend(request, "PUT", `/admin/workflow-definitions/${def.id}/approvers`, {
        version: approvers.version,
        approvers: [REQUESTER, APPROVER_A, APPROVER_B, PRINCIPAL].map((user) => ({ transitionKey: "submit", stepKey: "cab", source: "user", user })),
      });
      const now = Date.now();
      await apiSend(request, "POST", "/admin/approval-delegations", {
        principalUserId: ids[PRINCIPAL],
        delegateUserId: ids[DELEGATE],
        startsAt: new Date(now - 60_000).toISOString(),
        endsAt: new Date(now + 86_400_000).toISOString(),
        reason: "e2e: away",
      });

      for (const [n, label] of [[1, CI1], [2, CI2]] as const) {
        ci[n].id = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls.id, attributes: { name: label } })).id;
        ci[n].instanceId = (await apiSend<{ instance: { id: string } }>(request, "POST", "/workflow-instances", { definitionKey: WF_KEY, ciId: ci[n].id })).instance.id;
      }
      // CI 2 is requested through the API by the requester; CI 1 in the browser (first test).
      const req = await session(browser, REQUESTER);
      const inst = (await apiGet<{ instance: Instance }>(request, `/workflow-instances/${ci[2].instanceId}`)).instance;
      const res = await asUser(req, "POST", `/workflow-instances/${ci[2].instanceId}/transitions`, { transitionKey: "submit", expectedVersion: inst.version });
      expect(res.status(), `request CI 2: ${await res.text()}`).toBe(202);
      await req.context().close();
      const after = (await apiGet<{ instance: Instance }>(request, `/workflow-instances/${ci[2].instanceId}`)).instance;
      ci[2].requestId = after.pendingApproval!.requestId;
      ci[2].requestNo = (await stored(2)).requestNo;
    });

    test(`${locale}: the requester asks for approval and may not approve it, in the UI or the API (four-eyes)`, async ({ browser }) => {
      const page = await session(browser, REQUESTER);
      await openCi(page, 1);
      await panel(page).getByTestId("wf-transition-submit").click();
      const ask = page.getByRole("dialog", { name: L("wfRun.transition.title", { transition: "Submit", workflow: WF }) });
      await expect(ask.getByTestId("wf-requires-approval")).toContainText(L("approvalRun.requires.title"));
      await expect(ask.getByRole("list", { name: L("approvalRun.requires.steps") }).getByRole("listitem")).toHaveText([`CAB: ${L("approvalRun.requires.step", { n: 2 })}`]);
      await expect(ask).toContainText(L("approvalRun.requires.self"));
      await ask.getByRole("button", { name: L("approvalRun.requestSubmit") }).click();
      await expect(flash(page, L("approvalRun.requested", { transition: "Submit", ci: CI1 }))).toBeVisible();
      await expect(banner(page)).toContainText(progress(0));
      await expectNotClipped(banner(page));
      await expectNoEnglish(locale, banner(page));

      const inst = (await apiGet<{ instance: Instance }>(adminRequest!, `/workflow-instances/${ci[1].instanceId}`)).instance;
      expect(inst.state.key).toBe("draft");
      ci[1].requestId = inst.pendingApproval!.requestId;
      ci[1].requestNo = (await stored(1)).requestNo;

      // Listed as an approver of the step, the requester still gets no decision: the view button, no approve.
      await expect(banner(page).getByTestId("wf-pending-open")).toHaveText(L("approvalRun.banner.view"));
      await banner(page).getByTestId("wf-pending-open").click();
      const dlg = decisionDialog(page, CI1);
      await expect(dlg.getByTestId("approval-cannot-decide")).toContainText(L("approvalRun.refusal.requester"));
      await expect(dlg.getByRole("button", { name: L("approvalRun.decide.approveSubmit"), exact: true })).toHaveCount(0);
      await expect(dlg.getByRole("radio")).toHaveCount(0);
      await expectDialogLaidOut(dlg);
      await expectNoEnglish(locale, dlg);
      await snap(page, `aq-${locale}-requester-dialog`);
      await dlg.getByRole("button", { name: L("approvalRun.close") }).click();
      await expect(dlg).toBeHidden();

      // Neither of their own requests waits for them: no badge, an empty inbox.
      await page.goto("/approvals");
      await expect(page.getByTestId("approvals-empty")).toContainText(L("approvals.empty.actionable"));
      await expect(navBadge(page)).toHaveCount(0);
      expect(await myCount(page)).toBe(0);

      // The API refuses the requester's approval too, and records nothing.
      const r = await stored(1);
      const res = await asUser(page, "POST", `/workflow-approval-requests/${ci[1].requestId}/decisions`, { stepKey: "cab", decision: "approve", expectedVersion: r.version });
      expect(res.status()).toBe(403);
      const refused = (await res.json()) as { error: { code: string; details: { code: string }[] } };
      expect(refused.error.code).toBe("WORKFLOW_APPROVAL_SELF");
      expect(refused.error.details[0]?.code).toBe("requester");
      const after = await stored(1);
      expect(after.status).toBe("pending");
      expect(Number(after.steps[0].approvals)).toBe(0);
      expect(after.steps[0].decisions).toEqual([]);
      await done(page);
    });

    test(`${locale}: approver A sees 2 waiting, approves CI 1 from the inbox (1 of 2), and the count drops to 1`, async ({ browser }) => {
      const page = await session(browser, APPROVER_A);
      await page.goto("/approvals");
      await expect(navBadge(page)).toHaveText(new RegExp(`^2`));
      await expect(tabCount(page)).toHaveText("2");
      await expect(page.getByTestId("approvals-table").locator("tbody tr")).toHaveCount(2);
      const row = inboxRow(page, CI1);
      await expect(row).toContainText(L("approvalRun.history.step", { step: 1, steps: 1, name: "CAB", n: 0, required: 2 }));
      // The approvals UI and its nav entry; the inventory label's German truncation is GH#888.
      await expectNotClipped(page.locator("main"));
      await expectNotClipped(navLink(page));
      await expectNoEnglish(locale, page.locator("main"));
      await snap(page, `aq-${locale}-inbox-a`);

      await row.getByRole("button", { name: L("approvals.decideLabel", { no: ci[1].requestNo, ci: CI1 }) }).click();
      const dlg = decisionDialog(page, CI1);
      await expect(dlg.getByRole("group", { name: L("approvalRun.decide.legend", { step: "CAB" }) })).toBeVisible();
      // The first of two approvals: not the last one, so the dialog does not say the transition runs now.
      await expect(dlg).not.toContainText(L("approvalRun.decide.finalHint", { transition: "Submit", to: "Done" }));
      await expect(dlg.getByLabel(L("approvalRun.decide.onBehalf"))).toHaveCount(0);
      await expectDialogLaidOut(dlg);
      await expectNoEnglish(locale, dlg);
      await dlg.getByRole("button", { name: L("approvalRun.decide.approveSubmit"), exact: true }).click();
      await expect(flash(page, L("approvalRun.decided.approved", { step: "CAB", transition: "Submit", ci: CI1 }))).toBeVisible();

      await expect(navBadge(page)).toHaveText(/^1/);
      await expect(tabCount(page)).toHaveText("1");
      await expect(inboxRow(page, CI1)).toHaveCount(0);
      await expect(inboxRow(page, CI2)).toBeVisible();
      expect(await myCount(page)).toBe(1);

      const r = await stored(1);
      expect(r.status).toBe("pending");
      expect(Number(r.steps[0].approvals)).toBe(1);
      expect(r.steps[0].decisions.map((d) => [d.decision, d.actorName, d.onBehalfOfName])).toEqual([["approve", APPROVER_A, null]]);

      // On the CI page: 1 of 2, with A's approval listed.
      await openCi(page, 1);
      await expect(banner(page)).toContainText(progress(1));
      await banner(page).getByTestId("wf-pending-open").click();
      const view = decisionDialog(page, CI1);
      await expect(view.getByRole("table", { name: L("approvalRun.steps.label") }).getByRole("row").nth(1)).toContainText(L("approvalRun.steps.count", { n: 1, required: 2 }));
      await expect(view.getByRole("table", { name: L("approvalRun.steps.label") })).toContainText(`${L("approvalRun.decision.approved")} ${L("approvalRun.decision.by", { name: APPROVER_A })}`);
      await expectNoEnglish(locale, view);
      // Escape closes the read-only view (GH#887: A has decided).
      await page.keyboard.press("Escape");
      await expect(view).toBeHidden();

      await page.goto("/approvals?view=decided");
      await expect(inboxRow(page, CI1)).toContainText(L("approvalRun.status.pending"));
      await done(page);
    });

    // GH#887: once A decided, the API says already_decided and the banner offers only to view the request.
    test(`${locale}: approver A, having decided CI 1, is offered only to view it (API and banner)`, async ({ browser }) => {
      const page = await session(browser, APPROVER_A);
      const res = await asUser(page, "GET", `/workflow-approval-requests/${ci[1].requestId}`);
      const me = ((await res.json()) as { myEligibility: { canDecide: boolean; reason: string | null } }).myEligibility;
      expect([me.canDecide, me.reason]).toEqual([false, "already_decided"]);
      await openCi(page, 1);
      await expect(banner(page).getByTestId("wf-pending-open")).toHaveText(L("approvalRun.banner.view"));
      await banner(page).getByTestId("wf-pending-open").click();
      await expect(decisionDialog(page, CI1).getByTestId("approval-cannot-decide")).toBeVisible();
      await done(page);
    });

    test(`${locale}: the delegate approves CI 1 for P, the last approval, which runs the transition`, async ({ browser }) => {
      const page = await session(browser, DELEGATE);
      await page.goto("/approvals");
      // D is no approver: both requests reach them through P's delegation.
      await expect(navBadge(page)).toHaveText(/^2/);
      await expect(tabCount(page)).toHaveText("2");
      await inboxRow(page, CI1).getByRole("button", { name: L("approvals.decideLabel", { no: ci[1].requestNo, ci: CI1 }) }).click();
      const dlg = decisionDialog(page, CI1);
      await expect(dlg.getByLabel(L("approvalRun.decide.onBehalf")).locator("option")).toHaveText([L("approvalRun.decide.forPrincipal", { name: PRINCIPAL })]);
      await expect(dlg).toContainText(L("approvalRun.decide.finalHint", { transition: "Submit", to: "Done" }));
      await expectDialogLaidOut(dlg);
      await expectNoEnglish(locale, dlg);
      await snap(page, `aq-${locale}-delegate-dialog`);
      await dlg.getByRole("button", { name: L("approvalRun.decide.approveSubmit"), exact: true }).click();
      await expect(flash(page, L("approvalRun.decided.applied", { transition: "Submit", ci: CI1, state: "Done" }))).toBeVisible();
      await expect(navBadge(page)).toHaveText(/^1/);
      await expect(tabCount(page)).toHaveText("1");
      await expect(inboxRow(page, CI1)).toHaveCount(0);
      expect(await myCount(page)).toBe(1);

      const r = await stored(1);
      expect(r.status).toBe("approved");
      expect(Number(r.steps[0].approvals)).toBe(2);
      expect(r.steps[0].decisions.map((d) => [d.decision, d.actorName, d.onBehalfOfName])).toEqual(
        expect.arrayContaining([
          ["approve", APPROVER_A, null],
          ["approve", DELEGATE, PRINCIPAL],
        ]),
      );
      const inst = (await apiGet<{ instance: Instance }>(adminRequest!, `/workflow-instances/${ci[1].instanceId}`)).instance;
      expect(inst.state.key).toBe("done");
      expect(inst.pendingApproval).toBeNull();

      // The principal's own count drops as well: their approval was cast.
      const p = await session(browser, PRINCIPAL);
      expect(await myCount(p)).toBe(1);
      await p.goto("/approvals");
      await expect(tabCount(p)).toHaveText("1");
      await expect(inboxRow(p, CI2)).toBeVisible();
      await expect(inboxRow(p, CI1)).toHaveCount(0);
      await done(p);
      await done(page);
    });

    test(`${locale}: approver B rejects CI 2 from the CI page, with a reason; every count drops to 0`, async ({ browser }) => {
      const page = await session(browser, APPROVER_B);
      await page.goto("/");
      await expect(navBadge(page)).toHaveText(/^1/);
      await openCi(page, 2);
      await expect(banner(page)).toContainText(progress(0));
      await expect(banner(page)).toContainText(L("approvalRun.banner.requestedBy", { name: REQUESTER }));
      await banner(page).getByRole("button", { name: L("approvalRun.banner.decide") }).click();
      const dlg = decisionDialog(page, CI2);
      await dlg.getByLabel(L("approvalRun.decide.reject"), { exact: true }).check();
      await dlg.getByRole("button", { name: L("approvalRun.decide.rejectSubmit") }).click();
      await expect(dlg.locator("#approval-comment")).toHaveAttribute("aria-invalid", "true");
      await expect(dlg).toContainText(L("approvalRun.decide.commentMissing"));
      await expectNoEnglish(locale, dlg);
      await expectDialogLaidOut(dlg);
      // Nothing was sent: the request is unchanged.
      expect((await stored(2)).steps[0].decisions).toEqual([]);
      await dlg.locator("#approval-comment").fill("Not in the change window");
      await dlg.getByRole("button", { name: L("approvalRun.decide.rejectSubmit") }).click();
      await expect(flash(page, L("approvalRun.decided.rejected", { transition: "Submit", ci: CI2 }))).toBeVisible();
      await expect(banner(page)).toHaveCount(0);
      await expect(navBadge(page)).toHaveCount(0);
      expect(await myCount(page)).toBe(0);

      const r = await stored(2);
      expect(r.status).toBe("rejected");
      expect(r.steps[0].decisions.map((d) => [d.decision, d.actorName, d.comment])).toEqual([["reject", APPROVER_B, "Not in the change window"]]);
      const inst = (await apiGet<{ instance: Instance }>(adminRequest!, `/workflow-instances/${ci[2].instanceId}`)).instance;
      expect(inst.state.key).toBe("draft");
      expect(inst.pendingApproval).toBeNull();
      await done(page);

      // A and D had CI 2 waiting: it is gone from their inbox and badge.
      for (const u of [APPROVER_A, DELEGATE, PRINCIPAL]) {
        const other = await session(browser, u);
        expect(await myCount(other), `${u}'s count`).toBe(0);
        await other.goto("/approvals");
        await expect(other.getByTestId("approvals-empty")).toContainText(L("approvals.empty.actionable"));
        await expect(navBadge(other)).toHaveCount(0);
        await done(other);
      }
    });

    test(`${locale}: the requester finds both outcomes under Requested by me`, async ({ browser }) => {
      const page = await session(browser, REQUESTER);
      await page.goto("/approvals?view=requested");
      await expect(page.getByTestId("approvals-view-requested")).toHaveAttribute("aria-current", "page");
      await expect(inboxRow(page, CI1)).toContainText(L("approvalRun.status.approved"));
      await expect(inboxRow(page, CI2)).toContainText(L("approvalRun.status.rejected"));
      await expectNotClipped(page.locator("main"));
      await expectNoEnglish(locale, page.locator("main"));
      await snap(page, `aq-${locale}-requested`);
      await done(page);
    });
  });
}
