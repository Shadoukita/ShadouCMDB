import type { Browser, Page } from "@playwright/test";
import { STORAGE_STATE } from "./global-setup";
import { apiSend, checkA11y, chooseTheme, expect, snap, test } from "./support";

// Administration › Workflows › Designer in the new look (design document §1.12 N2, step 11-5c): the draft's toolbar,
// check panel, diagram, inspectors and tables, and the condition wording the read-only version view shares, in en and
// de. No inline styles.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const WF = `E2E designer ${stamp}`;
const WF_KEY = `e2e_designer_${stamp}`;
let wfId = "";

const GRAPH = {
  initialState: "draft",
  states: [
    { key: "draft", name: "Draft", category: "open" },
    { key: "review", name: "Review", category: "active" },
    { key: "done", name: "Done", category: "done", terminal: true },
  ],
  transitions: [
    { key: "submit", name: "Submit", from: "draft", to: "review", requiresComment: true, conditions: { all: [{ field: "name", op: "contains", value: "prod" }, { field: "name", op: "isSet" }] } },
    { key: "approve", name: "Approve", from: "review", to: "done" },
  ],
};

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Wf designer ${stamp}`, key: `wf_designer_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  wfId = (await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: WF, key: WF_KEY, classId: cls.id })).id;
  const draft = await apiSend<{ checksum: string }>(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, GRAPH);
  await apiSend(request, "POST", `/admin/workflow-definitions/${wfId}/draft/publish`, { expectedDraftChecksum: draft.checksum, changeNote: "First cut" });
  // Version 2 is the draft the designer opens.
  await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, GRAPH);
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

test("designer: toolbar, check, diagram, inspectors and tables", async ({ page }, testInfo) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByTestId("wf-save-state")).toHaveText("All changes saved");
  await expect(page.getByTestId("wf-lint").getByRole("heading", { name: "Check" })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Publish version 2/ })).toBeEnabled();
  await expect(page.getByRole("heading", { name: "States (3)" })).toBeVisible();
  const submit = page.getByRole("row", { name: /Submit/ });
  await expect(submit).toContainText("Comment. If Name contains prod and Name is set.");
  expect(await page.locator("main [style]").count(), "no inline styles").toBe(0);

  await page.getByRole("button", { name: "Transition Submit: Draft to Review" }).click();
  const inspector = page.getByRole("heading", { name: "Transition: Submit" });
  await expect(inspector).toBeVisible();
  await expect(page.getByLabel("Condition 1: comparison")).toHaveValue("contains");
  await expect(page.getByLabel("Condition 2: comparison").locator("option:checked")).toHaveText("is set");
  await checkA11y(page, testInfo, "admin-workflow-designer-transition", { include: "main" });

  await page.getByRole("button", { name: "State Done, Done, terminal" }).click();
  await expect(page.getByRole("heading", { name: "State: Done" })).toBeVisible();
  await expect(page.getByText("1 incoming, 0 outgoing transitions.")).toBeVisible();
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-workflow-designer-state-dark", { include: "main" });
  await chooseTheme(page, "");
});

test("designer and version view: the texts and the condition wording come from the German catalog", async ({ browser }) => {
  const page = await open(browser, { locale: "de" });
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await expect(page.getByTestId("wf-save-state")).toHaveText("Alle Änderungen gespeichert");
  await expect(page.getByRole("button", { name: "+ Status" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Version 2 veröffentlichen…" })).toBeVisible();
  await expect(page.getByTestId("wf-lint").getByRole("heading", { name: "Prüfung" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Erfordert" })).toBeVisible();
  await expect(page.getByRole("row", { name: /Submit/ })).toContainText("Kommentar. Wenn Name enthält prod und Name ist gesetzt.");

  await page.getByRole("button", { name: "Übergang Submit: Draft nach Review" }).click();
  await expect(page.getByRole("heading", { name: "Übergang: Submit" })).toBeVisible();
  await expect(page.getByLabel("Zum Ausführen ist ein Kommentar erforderlich")).toBeChecked();
  await expect(page.getByRole("button", { name: "+ Bedingung" })).toBeVisible();
  await expect(page.getByLabel("Bedingung 2: Vergleich").locator("option:checked")).toHaveText("ist gesetzt");

  await page.getByRole("button", { name: "Status Draft, Offen, Anfangsstatus" }).click();
  await expect(page.getByRole("heading", { name: "Status: Draft" })).toBeVisible();
  await expect(page.getByText("0 eingehende, 1 ausgehender Übergang.")).toBeVisible();

  await page.getByRole("tab", { name: "Versionen" }).click();
  await page.getByTestId("wf-version-1").getByRole("button", { name: "Ansehen" }).click();
  await expect(page.getByRole("cell", { name: "name enthält prod und name ist gesetzt" })).toBeVisible();
  await page.context().close();
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): the designer with a transition and a state selected, and a
// version's read-only view, × both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`designer screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ browser }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          const page = await open(browser, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
            width,
          });
          const suffix = `${locale}-${theme}-${density}-${width}`;
          await page.goto(`/admin/workflows/${wfId}?tab=designer`);
          await expect(page.getByTestId("wf-lint")).toBeVisible();
          await page.locator(".wf-edge").first().click();
          await expect(page.locator(".wf-inspector")).toBeVisible();
          await snap(page, `wf-designer-transition-${suffix}`);
          await page.locator(".wf-node").last().click();
          await expect(page.locator(".wf-inspector")).toBeVisible();
          await snap(page, `wf-designer-state-${suffix}`);
          await page.goto(`/admin/workflows/${wfId}?tab=versions`);
          await page.getByTestId("wf-version-1").getByRole("button").first().click();
          await expect(page.locator(".wf-canvas")).toBeVisible();
          await snap(page, `wf-designer-version-${suffix}`);
          await page.context().close();
        });
