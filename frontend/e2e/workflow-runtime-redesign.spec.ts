import type { Browser, Page } from "@playwright/test";
import { STORAGE_STATE } from "./global-setup";
import { apiGet, apiSend, checkA11y, chooseTheme, expect, snap, test } from "./support";

// The workflow screens an operator uses, in the reference-mockup look (design document §1.12 N2, step 11-5a): the
// Workflow instances list with the inventory's head band (breadcrumb, title with the count, intro, filters) above
// the summary and the table card; an instance with the record head band (state and status pills, the CI as a chip,
// Force state); and the CI's Workflows tab. No inline styles, and every text in en and de.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const WF = `E2E redesign flow ${stamp}`;
const WF_KEY = `e2e_redesign_${stamp}`;
const CI = `wf-redesign-ci-${stamp}`;
let ciId = "";
let instanceId = "";

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Wf redesign ${stamp}`, key: `wf_redesign_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  const def = await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: WF, key: WF_KEY, classId: cls.id });
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${def.id}/draft`, {
    initialState: "draft",
    states: [
      { key: "draft", name: "Draft", category: "open" },
      { key: "review", name: "Review", category: "active" },
      { key: "done", name: "Done", category: "done", terminal: true },
    ],
    transitions: [
      { key: "submit", name: "Submit", from: "draft", to: "review" },
      { key: "approve", name: "Approve", from: "review", to: "done" },
    ],
  });
  await apiSend(request, "POST", `/admin/workflow-definitions/${def.id}/draft/publish`, { expectedDraftChecksum: draft.checksum });
  const cur = await apiGet<{ version: number }>(request, `/admin/workflow-definitions/${def.id}`);
  await apiSend(request, "PATCH", `/admin/workflow-definitions/${def.id}`, { version: cur.version, isActive: true });
  ciId = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls.id, attributes: { name: CI } })).id;
  const started = await apiSend<{ instance: { id: string; version: number } }>(request, "POST", "/workflow-instances", { definitionKey: WF_KEY, ciId });
  instanceId = started.instance.id;
  await apiSend(request, "POST", `/workflow-instances/${instanceId}/transitions`, {
    transitionKey: "submit",
    expectedVersion: started.instance.version,
    fields: {},
    comment: "Ready for review",
  });
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

test("workflow instances: head band, summary, status pills, numbered pages", async ({ page }, testInfo) => {
  await page.goto(`/workflows?workflow=${WF_KEY}`);
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Workflows");
  await expect(head.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText("1 instance");
  await expect(head.getByLabel("Workflow", { exact: true })).toHaveValue(WF_KEY);
  await expect(head.getByRole("button", { name: "Clear filters" })).toBeVisible();

  const summary = page.getByTestId("wf-summary");
  await expect(summary.getByRole("rowheader", { name: new RegExp(WF) })).toBeVisible();
  await expect(summary.getByRole("button", { name: /Review/ })).toHaveAttribute("aria-pressed", "false");

  const list = page.getByRole("region", { name: "Workflow instances" });
  await expect(list.locator("table.list-table th").first()).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row", { name: new RegExp(CI) });
  await expect(row.getByRole("link", { name: CI })).toHaveClass(/\blist-name\b/);
  const status = row.locator(".badge").filter({ hasText: "Running" });
  await expect(status).toHaveCSS("border-radius", "999px");
  await expect(status.locator("svg")).toHaveCount(1);
  await expect(list.locator(".table-footer")).toBeVisible();
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  // A count filters the list to the running instances in that state, in the URL.
  await summary.getByRole("button", { name: /Review/ }).click();
  await expect(page).toHaveURL(/state=review/);
  await expect(summary.getByRole("button", { name: /Review/ })).toHaveAttribute("aria-pressed", "true");

  await checkA11y(page, testInfo, "workflow-instances-light", { include: "main" });
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "workflow-instances-dark", { include: "main" });
  await chooseTheme(page, "");
});

test("workflow instance: record head band with pills, the CI chip and Force state", async ({ page }, testInfo) => {
  await page.goto(`/workflows/${instanceId}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(WF);
  await expect(head.getByRole("heading", { level: 1, name: WF })).toBeVisible();
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge").filter({ hasText: "Review" })).toBeVisible();
  await expect(meta.locator(".badge").filter({ hasText: "Running" }).locator("svg")).toHaveCount(1);
  await expect(meta.getByRole("link", { name: CI })).toHaveClass(/\brecord-class-chip\b/);
  await expect(meta).toContainText("v1");
  await expect(head.getByRole("button", { name: "Force state…" })).toBeVisible();

  await expect(page.locator("tr[aria-current=step]")).toContainText("Review");
  const events = page.getByTestId("wf-events");
  await expect(events).toContainText("2 events, oldest first");
  await expect(events).toContainText("Ready for review");
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  // The force dialog: its texts, a missing reason next to the field.
  await head.getByRole("button", { name: "Force state…" }).click();
  const dialog = page.getByRole("dialog", { name: "Force a state" });
  await dialog.getByRole("button", { name: "Force state" }).click();
  await expect(dialog.getByText("Give a reason.")).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await checkA11y(page, testInfo, "workflow-instance-light", { include: "main" });
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "workflow-instance-dark", { include: "main" });
  await chooseTheme(page, "");
});

test("the CI's Workflows tab: status pills, actions and the start dialog", async ({ page }, testInfo) => {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: "Workflows" }).click();
  const panel = page.getByTestId("ci-workflows");
  const row = panel.getByRole("row").filter({ hasText: WF });
  await expect(row.locator(".badge").filter({ hasText: "Running" }).locator("svg")).toHaveCount(1);
  await expect(row.getByRole("button", { name: "Approve" })).toBeVisible();
  await expect(row.getByRole("button", { name: "Cancel workflow" })).toBeVisible();
  expect(await panel.locator("[style]").count(), "no inline styles").toBe(0);

  await row.getByRole("button", { name: "Cancel workflow" }).click();
  const dialog = page.getByRole("dialog", { name: `Cancel ${WF}?` });
  await expect(dialog).toContainText(`The workflow stops in Review on ${CI}.`);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await checkA11y(page, testInfo, "ci-workflows-tab", { include: "main" });
});

test("workflow screens: the texts come from the German catalog", async ({ browser }) => {
  const page = await open(browser, { locale: "de" });
  await page.goto(`/workflows?workflow=${WF_KEY}`);
  const head = page.locator(".list-head");
  await expect(head.locator(".count")).toHaveText("1 Instanz");
  await expect(head.getByRole("button", { name: "Filter zurücksetzen" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "Laufen gerade" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Letzter Schritt" })).toBeVisible();
  await expect(page.getByRole("row", { name: new RegExp(CI) }).locator(".badge").filter({ hasText: "Läuft" })).toBeVisible();

  await page.goto(`/workflows/${instanceId}`);
  await expect(page.getByRole("button", { name: "Zustand erzwingen…" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "Nächste Schritte" })).toBeVisible();
  await expect(page.getByTestId("wf-events")).toContainText("2 Ereignisse, älteste zuerst");
  await expect(page.getByTestId("wf-events")).toContainText("Gestartet");
  await expect(page.getByRole("button", { name: "Workflow abbrechen" })).toBeVisible();

  await page.goto("/workflows/00000000-0000-0000-0000-000000000000");
  await expect(page.getByRole("heading", { name: "Workflow-Instanz nicht gefunden" })).toBeVisible();
  await page.context().close();
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): three screens × both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`workflow screens screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ browser }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          const page = await open(browser, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
            width,
          });
          const suffix = `${locale}-${theme}-${density}-${width}`;
          await page.goto(`/workflows?workflow=${WF_KEY}`);
          await expect(page.getByTestId("wf-summary").locator("table")).toBeVisible();
          await snap(page, `wf-instances-${suffix}`);
          await page.goto(`/workflows/${instanceId}`);
          await expect(page.getByTestId("wf-events").locator("table")).toBeVisible();
          await snap(page, `wf-instance-${suffix}`);
          await page.goto(`/cis/${ciId}`);
          await page.getByRole("tab", { name: /Workflows/ }).click();
          await expect(page.getByTestId("ci-workflows").locator("table")).toBeVisible();
          await snap(page, `wf-ci-tab-${suffix}`);
          await page.context().close();
        });
