import type { Browser, Page } from "@playwright/test";
import { STORAGE_STATE } from "./global-setup";
import { apiGet, apiSend, checkA11y, chooseTheme, expect, snap, test } from "./support";

// Administration › Workflows in the reference-mockup look (design document §1.12 N2, step 11-5b): the list with the
// inventory's head band and numbered pages; a workflow with the record head band (status and version chips, the tabs
// on its lower edge); Settings, Grants, Approvers and Notifications saved through the shared save bar with a toast;
// Versions with status pills and the migrate panel. No inline styles, and every text in en and de.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const WF = `E2E admin flow ${stamp}`;
const WF_KEY = `e2e_admin_${stamp}`;
const PROFILE = `E2E wf admin ${stamp}`;
let wfId = "";

const GRAPH = (extra: { key: string; name: string; from: string; to: string }[] = []) => ({
  initialState: "draft",
  states: [
    { key: "draft", name: "Draft", category: "open" },
    { key: "review", name: "Review", category: "active" },
    { key: "done", name: "Done", category: "done", terminal: true },
  ],
  transitions: [{ key: "submit", name: "Submit", from: "draft", to: "review" }, { key: "approve", name: "Approve", from: "review", to: "done" }, ...extra],
});

async function publish(request: Parameters<typeof apiSend>[0], graph: ReturnType<typeof GRAPH>, note: string) {
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, graph);
  await apiSend(request, "POST", `/admin/workflow-definitions/${wfId}/draft/publish`, { expectedDraftChecksum: draft.checksum, changeNote: note });
}

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Wf admin ${stamp}`, key: `wf_admin_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  await apiSend(request, "POST", "/admin/profiles", { name: PROFILE, description: "Runs the admin flow", globalPermissions: [], classPermissions: [] });
  wfId = (await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: WF, key: WF_KEY, classId: cls.id, description: "Redesign walk" })).id;
  await publish(request, GRAPH(), "First cut");
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${wfId}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${wfId}`, { version: cur.version, isActive: true });
  const ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls.id, attributes: { name: `wf-admin-ci-${stamp}` } })).id;
  await apiSend(request, "POST", "/workflow-instances", { definitionKey: WF_KEY, ciId });
  // Version 2 is current, version 1 keeps the running instance: it offers the migration.
  await publish(request, GRAPH([{ key: "reopen", name: "Reopen", from: "review", to: "draft" }]), "Reopen added");
});

async function open(browser: Browser, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de"; width?: number } = {}): Promise<Page> {
  const context = await browser.newContext({ storageState: STORAGE_STATE, viewport: { width: opts.width ?? 1440, height: 900 } });
  const page = await context.newPage();
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  return page;
}

test("workflows list: head band, status pills, numbered pages", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows?q=${encodeURIComponent(WF)}`);
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Administration");
  await expect(head.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText("1 workflow");
  await expect(head.getByRole("link", { name: "New workflow" })).toBeVisible();
  await expect(head.getByRole("button", { name: "Clear filters" })).toBeVisible();

  const list = page.getByRole("region", { name: "Workflows" });
  const row = list.getByRole("row", { name: new RegExp(WF) });
  await expect(row.getByRole("link", { name: WF })).toHaveClass(/\blist-name\b/);
  const status = row.locator(".badge").filter({ hasText: "Active" });
  await expect(status).toHaveCSS("border-radius", "999px");
  await expect(status.locator(".status-dot")).toHaveCount(1);
  await expect(list.locator(".table-footer")).toBeVisible();
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  await checkA11y(page, testInfo, "admin-workflows-light", { include: "main" });
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-workflows-dark", { include: "main" });
  await chooseTheme(page, "");
});

test("a workflow: record head band, tabs in the band, settings saved through the save bar", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(WF);
  await expect(head.getByRole("heading", { level: 1, name: WF })).toBeVisible();
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge").filter({ hasText: "Active" }).locator(".status-dot")).toHaveCount(1);
  await expect(meta).toContainText("v2 current");
  await expect(meta).toContainText(WF_KEY);
  await expect(head.getByRole("link", { name: "Open instances" })).toHaveAttribute("href", `/workflows?workflow=${WF_KEY}`);
  await expect(head.getByRole("tablist", { name: "Workflow sections" }).getByRole("tab", { name: "Settings" })).toHaveAttribute("aria-selected", "true");
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  const bar = page.getByRole("region", { name: "Save" });
  await expect(bar.getByRole("button", { name: "Save changes" })).toBeVisible();
  await expect(bar).not.toContainText("Unsaved changes");
  await page.getByLabel("Description").fill(`Redesign walk ${stamp}`);
  await expect(bar).toContainText("Unsaved changes");
  await expect(bar).toContainText("1 field changed");
  await checkA11y(page, testInfo, "admin-workflow-settings-dirty", { include: "main" });
  await bar.getByRole("button", { name: "Discard" }).click();
  await expect(page.getByLabel("Description")).toHaveValue("Redesign walk");
  await page.getByLabel("Description").fill(`Redesign walk ${stamp}`);
  await bar.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${WF}.` })).toBeVisible();
  await expect(bar).not.toContainText("Unsaved changes");

  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-workflow-settings-dark", { include: "main" });
  await chooseTheme(page, "");
});

test("versions: status pills, the migrate panel and the read-only view", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=versions`);
  const v1 = page.getByTestId("wf-version-1");
  const v2 = page.getByTestId("wf-version-2");
  await expect(v2.locator(".badge").filter({ hasText: "Published" }).locator(".status-dot")).toHaveCount(1);
  await expect(v2.locator(".badge").filter({ hasText: "Current" })).toBeVisible();
  await expect(page.locator(".panel-header .meta")).toHaveText("2 versions");
  await v1.getByRole("button", { name: "Migrate instances…" }).click();
  const migrate = page.getByTestId("wf-migrate");
  await expect(migrate.getByRole("heading", { name: "Migrate instances from version 1" })).toBeVisible();
  await expect(migrate.getByLabel("Target version")).toContainText("Version 2 (current)");
  await migrate.getByRole("button", { name: "Preview migration" }).click();
  await expect(migrate.getByTestId("wf-migrate-summary")).toContainText("1 to version 2");
  await expect(migrate.getByRole("button", { name: "Migrate 1 instance…" })).toBeVisible();
  await v2.getByRole("button", { name: "View" }).click();
  await expect(page.getByRole("heading", { name: "Version 2" })).toBeVisible();
  await expect(page.getByText("States: Draft (Open), Review (In progress), Done (Done, terminal). Initial: Draft.")).toBeVisible();
  await checkA11y(page, testInfo, "admin-workflow-versions", { include: "main" });
});

test("grants and notifications: the save bar and a toast", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=grants`);
  const matrix = page.getByTestId("wf-grants");
  const bar = page.getByRole("region", { name: "Save" });
  await expect(bar.getByRole("button", { name: "Save grants" })).toBeDisabled();
  await matrix.getByRole("checkbox", { name: `${PROFILE} may run Submit` }).check();
  await expect(bar).toContainText("Unsaved changes");
  await checkA11y(page, testInfo, "admin-workflow-grants-dirty", { include: "main" });
  await bar.getByRole("button", { name: "Save grants" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Grants saved." })).toBeVisible();
  await expect(bar).not.toContainText("Unsaved changes");
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  await page.getByRole("tab", { name: "Notifications" }).click();
  await expect(page).toHaveURL(/tab=actions/);
  const actionsBar = page.getByRole("region", { name: "Save" });
  await expect(actionsBar.getByRole("button", { name: "Save notifications" })).toBeDisabled();
  await page.getByRole("button", { name: "+ Notification" }).click();
  await expect(actionsBar).toContainText("Unsaved changes");
  await actionsBar.getByRole("button", { name: "Discard" }).click();
  await expect(actionsBar).not.toContainText("Unsaved changes");
  await checkA11y(page, testInfo, "admin-workflow-actions", { include: "main" });
});

test("notifications: a webhook picks an active endpoint; the key is typed when the list cannot load", async ({ page }, testInfo) => {
  // CI runs with webhooks off, where no endpoint can be registered: serve the list a workflows.manage caller gets.
  const limited = { suspendedReason: null, url: null, unencrypted: null, payloadVersion: null, timeoutMs: null };
  const endpoints = [
    { id: "00000000-0000-4000-8000-000000000001", key: "itsm", name: "ITSM", status: "active", ...limited },
    { id: "00000000-0000-4000-8000-000000000002", key: "monitoring", name: "Monitoring", status: "paused", ...limited },
    { id: "00000000-0000-4000-8000-000000000003", key: "siem", name: "SIEM", status: "suspended", ...limited },
  ];
  let fail = false;
  await page.route("**/api/v1/admin/webhook-endpoints?*", (route) =>
    fail
      ? route.fulfill({ status: 500, json: { error: { code: "INTERNAL_ERROR", message: "Internal error", requestId: "e2e" } } })
      : route.fulfill({ json: { data: endpoints, page: { limit: 200, offset: 0, total: 3 } } }),
  );
  await page.goto(`/admin/workflows/${wfId}?tab=actions`);
  await page.getByRole("button", { name: "+ Notification" }).click();
  const editor = page.getByTestId("wf-action-editor-0");
  await editor.getByLabel("Kind").selectOption("webhook");
  const picker = editor.getByLabel("Endpoint", { exact: true });
  await expect(picker).toBeEnabled();
  await expect(picker.locator("option")).toHaveText(["Choose…", "ITSM (itsm)", "Monitoring (monitoring) · paused", "SIEM (siem) · suspended"]);
  await expect(picker.locator("option:disabled")).toHaveText(["Choose…", "Monitoring (monitoring) · paused", "SIEM (siem) · suspended"]);
  await picker.selectOption("itsm");
  await expect(page.getByTestId("wf-action-row-notify")).toContainText("ITSM (itsm)");
  await expect(editor.getByTestId("wf-action-endpoint-hint")).toContainText("Only active endpoints can be chosen.");
  await checkA11y(page, testInfo, "admin-workflow-action-endpoint", { include: "main" });

  // The list failing does not block the form: the key is typed as before, with a retry.
  fail = true;
  await page.getByRole("region", { name: "Save" }).getByRole("button", { name: "Discard" }).click();
  await page.reload();
  await page.getByRole("button", { name: "+ Notification" }).click();
  await page.getByTestId("wf-action-editor-0").getByLabel("Kind").selectOption("webhook");
  const typed = page.getByTestId("wf-action-editor-0").getByRole("textbox", { name: "Endpoint", exact: true });
  // The client retries a 5xx twice before giving up.
  await expect(typed).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("wf-action-editor-0").getByRole("alert")).toContainText("The webhook endpoints could not be loaded.");
  await typed.fill("itsm");
  await page.getByRole("region", { name: "Save" }).getByRole("button", { name: "Discard" }).click();
});

test("administration › workflows: the texts come from the German catalog", async ({ browser }) => {
  const page = await open(browser, { locale: "de" });
  await page.goto(`/admin/workflows?q=${encodeURIComponent(WF)}`);
  await expect(page.locator(".list-head .count")).toHaveText("1 Workflow");
  await expect(page.getByRole("link", { name: "Neuer Workflow" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Zustandsfeld" })).toBeVisible();

  await page.goto(`/admin/workflows/${wfId}`);
  await expect(page.getByTestId("record-meta")).toContainText("v2 aktuell");
  await expect(page.getByRole("tab", { name: "Einstellungen" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("heading", { level: 2, name: "Eckdaten" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Workflow löschen" })).toBeVisible();
  await expect(page.getByLabel("Eine Instanz starten, wenn ein CI des Typs angelegt wird")).toBeVisible();

  await page.getByRole("tab", { name: "Versionen" }).click();
  await expect(page.getByTestId("wf-version-2")).toContainText("Veröffentlicht");
  await expect(page.getByTestId("wf-version-1").getByRole("button", { name: "Instanzen migrieren…" })).toBeVisible();

  await page.getByRole("tab", { name: "Berechtigungen" }).click();
  await expect(page.getByRole("button", { name: "Berechtigungen speichern" })).toBeDisabled();
  await expect(page.getByRole("columnheader", { name: "Übergang" })).toBeVisible();
  await expect(page.getByTestId("wf-grants").getByRole("rowheader", { name: "Eine Instanz abbrechen" })).toBeVisible();

  await page.goto("/admin/workflows/00000000-0000-0000-0000-000000000000");
  await expect(page.getByRole("heading", { name: "Workflow nicht gefunden" })).toBeVisible();
  await page.context().close();
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): five screens × both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`administration › workflows screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ browser }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          const page = await open(browser, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
            width,
          });
          const suffix = `${locale}-${theme}-${density}-${width}`;
          await page.goto(`/admin/workflows?q=${encodeURIComponent(WF)}`);
          await expect(page.locator("table.list-table")).toBeVisible();
          await snap(page, `wf-admin-list-${suffix}`);
          await page.goto(`/admin/workflows/${wfId}`);
          await expect(page.locator("#wf-settings-form")).toBeVisible();
          await snap(page, `wf-admin-settings-${suffix}`);
          await page.goto(`/admin/workflows/${wfId}?tab=versions`);
          await page.getByTestId("wf-version-1").getByRole("button").nth(1).click();
          await expect(page.getByTestId("wf-migrate").locator("table")).toBeVisible();
          await snap(page, `wf-admin-versions-${suffix}`);
          await page.goto(`/admin/workflows/${wfId}?tab=grants`);
          await expect(page.getByTestId("wf-grants")).toBeVisible();
          await snap(page, `wf-admin-grants-${suffix}`);
          await page.goto(`/admin/workflows/${wfId}?tab=actions`);
          await expect(page.getByTestId("wf-actions")).toBeVisible();
          await snap(page, `wf-admin-actions-${suffix}`);
          await page.context().close();
        });
