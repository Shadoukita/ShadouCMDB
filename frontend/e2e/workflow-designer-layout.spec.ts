import { apiSend, classIdByName, csrf, expect, test } from "./support";

// The workflow designer at a laptop width (GH#644): the diagram gets the full width with the inspector under it,
// arranged arrows leave their labels clear of the boxes, and the tables wrap long names. Runs on the demo seed.
const stamp = Date.now().toString(36);
const long = (s: string) => `${s}: a deliberately long state name for the layout check`;
let wfId = "";

test.use({ viewport: { width: 1280, height: 800 } });

test.beforeAll(async ({ request }) => {
  const classId = await classIdByName(request, "Server");
  wfId = (await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: `E2E layout ${stamp}`, key: `e2e_layout_${stamp}`, classId })).id;
  await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, {
    initialState: "planned",
    states: [
      { key: "planned", name: long("Planned"), category: "open" },
      { key: "in_service", name: long("In service"), category: "active" },
      { key: "maintenance", name: long("In maintenance"), category: "active" },
      { key: "retired", name: long("Retired"), category: "done", terminal: true },
    ],
    transitions: [
      { key: "commission", name: "Commission", from: "planned", to: "in_service" },
      { key: "start_maintenance", name: "Start maintenance", from: "in_service", to: "maintenance" },
      { key: "end_maintenance", name: "End maintenance", from: "maintenance", to: "in_service" },
      { key: "decommission", name: "Decommission", from: "in_service", to: "retired" },
    ],
    layout: {},
  });
});

test.afterAll(async ({ request }) => {
  if (wfId) await request.delete(`/api/v1/admin/workflow-definitions/${wfId}`, { headers: { "X-CSRF-Token": await csrf(request) } });
});

test("at 1280 px the diagram takes the full width and its labels stay clear of the states", async ({ page }) => {
  await page.goto(`/admin/workflows/${wfId}?tab=designer`);
  await page.getByRole("button", { name: "Arrange" }).click();
  await expect(page.getByTestId("wf-save-state")).toHaveText("All changes saved");

  const canvas = (await page.locator(".wf-canvas").boundingBox())!;
  expect(canvas.width).toBeGreaterThan(700);
  // Some states lie beyond the panel: the canvas says it scrolls.
  await expect(page.getByTestId("wf-canvas-hint")).toBeVisible();

  const boxes = await page.locator(".wf-node-box").evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON() as DOMRect));
  const labels = await page.locator(".wf-edge-label").evaluateAll((els) => els.map((e) => ({ text: e.textContent!.trim(), r: e.getBoundingClientRect().toJSON() as DOMRect })));
  expect(labels.map((l) => l.text).sort()).toEqual(["Commission", "Decommission", "End maintenance", "Start maintenance"]);
  const overlaps = (a: DOMRect, b: DOMRect) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
  for (const l of labels) {
    for (const b of boxes) expect(overlaps(l.r, b), `${l.text} overlaps a state`).toBe(false);
    for (const o of labels) if (o !== l) expect(overlaps(l.r, o.r), `${l.text} overlaps ${o.text}`).toBe(false);
  }

  // Tables: no horizontal overflow, and a transition's key is apart from its name.
  for (const table of await page.locator(".wf-designer-wrap ~ .grid-2 table.data").all()) {
    expect(await table.evaluate((t) => t.scrollWidth <= t.parentElement!.clientWidth)).toBe(true);
  }
  await expect(page.locator("td.wf-name-cell").filter({ hasText: "Commission" }).first()).toHaveText(/^\s*Commission\s+commission/);
});
