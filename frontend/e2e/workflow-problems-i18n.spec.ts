import type { Browser, Page } from "@playwright/test";
import { STORAGE_STATE } from "./global-setup";
import { apiSend, checkA11y, expect, test } from "./support";

// Administration › Workflows › Designer: the lint's problems in the user's language (SHAA-3003, GH#870). The check
// panel, the inspector and the publish dialog word each problem from its code and params, not from the API's English
// message.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
let wfId = "";

// "limbo" is unreachable and a dead end (errors); no profile is granted "submit" (a warning).
const BROKEN = {
  initialState: "draft",
  states: [
    { key: "draft", name: "Draft", category: "open" },
    { key: "done", name: "Done", category: "done", terminal: true },
    { key: "limbo", name: "Limbo", category: "active" },
  ],
  transitions: [{ key: "submit", name: "Submit", from: "draft", to: "done" }],
};

test.beforeAll(async ({ request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name: `Wf problems ${stamp}`, key: `wf_problems_${stamp}` });
  const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "name", label: "Name", dataType: "text" });
  await apiSend(request, "PATCH", `/ci-classes/${cls.id}`, { titleAttributeId: title.id });
  wfId = (await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: `E2E problems ${stamp}`, key: `e2e_problems_${stamp}`, classId: cls.id })).id;
  await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, BROKEN);
});

async function open(browser: Browser, locale?: "de"): Promise<Page> {
  const context = await browser.newContext({ storageState: STORAGE_STATE, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  if (locale) await page.addInitScript((l) => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = l), locale);
  return page;
}

test("German: the check panel and the inspector word errors and warnings from the catalog", async ({ browser }, testInfo) => {
  const page = await open(browser, "de");
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  const check = page.getByTestId("wf-lint");
  await expect(check.getByText("Der Zustand limbo ist vom Anfangszustand aus nicht erreichbar.")).toBeVisible();
  await expect(check.getByText("Der Zustand limbo ist kein Endzustand und hat keinen ausgehenden Übergang.")).toBeVisible();
  await expect(check.getByText("Kein Profil hat das Recht für den Übergang submit: Nur Administratoren könnten ihn ausführen.")).toBeVisible();
  await expect(check.getByText("2 Fehler")).toBeVisible();
  // None of the API's English messages.
  await expect(check).not.toContainText("cannot be reached");
  await expect(check).not.toContainText("No profile is granted");

  // The inspector of the state lists its own problems the same way.
  await check.getByRole("button", { name: "Zustand limbo" }).first().click();
  const problems = page.getByRole("list", { name: "Probleme" });
  await expect(problems.getByText("Der Zustand limbo ist vom Anfangszustand aus nicht erreichbar.")).toBeVisible();
  await checkA11y(page, testInfo, "admin-workflow-designer-problems-de", { include: "main" });
  await page.context().close();
});

test("English: the same problems read as before", async ({ page }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  const check = page.getByTestId("wf-lint");
  await expect(check.getByText("State limbo cannot be reached from the initial state.")).toBeVisible();
  await expect(check.getByText("No profile is granted transition submit: only administrators could run it.")).toBeVisible();
});

test("German: the publish dialog lists the warnings in German", async ({ browser, request }) => {
  // Without limbo the draft publishes; the ungranted transition stays a warning.
  await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, { ...BROKEN, states: BROKEN.states.slice(0, 2) });
  const page = await open(browser, "de");
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: /veröffentlichen…$/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("Kein Profil hat das Recht für den Übergang submit: Nur Administratoren könnten ihn ausführen.")).toBeVisible();
  await expect(dialog).not.toContainText("No profile is granted");
  await page.context().close();
});
