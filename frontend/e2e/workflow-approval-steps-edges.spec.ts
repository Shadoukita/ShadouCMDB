import type { Page } from "@playwright/test";
import { apiGet, apiSend, classIdByName, csrf, expect, test } from "./support";

// QA edge cases of the approval steps editor (SHAA-2849, #837), in en and de: a new step gets a default name and a
// free key; at most 5 steps; a duplicate key, an empty name and a quorum out of range are marked on the step in the
// page's language and nothing is saved; moving and removing steps saves them in their new order; removing the last
// step saves the transition without approval. Each language builds its own draft on the demo seed's Server type.
const stamp = Date.now().toString(36);

type Draft = { transitions: { key: string; approval?: { steps: { key: string; name: string; requiredApprovals: number }[] } | null }[] };

const L = {
  en: {
    none: "No approval: the transition runs as soon as it is started.",
    require: "Require approval",
    addStep: "+ Step",
    defaultName: (n: number) => `Step ${n}`,
    max: "At most 5 steps.",
    name: "Step name",
    key: "Step key",
    required: "Approvals required",
    duplicate: (key: string) => `Another step already has the key ${key}.`,
    needsName: (n: number) => `Step ${n} needs a name.`,
    range: (n: number) => `Step ${n}: between 1 and 20 approvals.`,
    down: (n: number) => `Move step ${n} down`,
    remove: (n: number) => `Remove step ${n}`,
    saved: "All changes saved",
    fix: "Not saved: fix the marked problems",
  },
  de: {
    none: "Keine Genehmigung: Der Übergang wird sofort ausgeführt.",
    require: "Genehmigung verlangen",
    addStep: "+ Stufe",
    defaultName: (n: number) => `Stufe ${n}`,
    max: "Höchstens 5 Stufen.",
    name: "Name der Stufe",
    key: "Schlüssel der Stufe",
    required: "Erforderliche Genehmigungen",
    duplicate: (key: string) => `Eine andere Stufe hat bereits den Schlüssel ${key}.`,
    needsName: (n: number) => `Stufe ${n} braucht einen Namen.`,
    range: (n: number) => `Stufe ${n}: zwischen 1 und 20 Genehmigungen.`,
    down: (n: number) => `Stufe ${n} nach unten`,
    remove: (n: number) => `Stufe ${n} entfernen`,
    saved: "Alle Änderungen gespeichert",
    fix: "Nicht gespeichert: Beheben Sie die markierten Probleme",
  },
} as const;

const saveState = (page: Page) => page.getByTestId("wf-save-state");
const approval = (page: Page) => page.locator(".wf-inspector").getByTestId("wf-approval");
const step = (page: Page, j: number) => approval(page).getByTestId(`wf-step-${j}`);

for (const lang of ["en", "de"] as const) {
  const m = L[lang];
  let wfId = "";

  test.describe(`approval steps editor (${lang})`, () => {
    test.beforeAll(async ({ request }) => {
      const classId = await classIdByName(request, "Server");
      wfId = (await apiSend<{ id: string }>(request, "POST", "/admin/workflow-definitions", { name: `E2E steps ${lang} ${stamp}`, key: `e2e_steps_${lang}_${stamp}`, classId })).id;
      await apiSend(request, "PUT", `/admin/workflow-definitions/${wfId}/draft`, {
        initialState: "planned",
        states: [
          { key: "planned", name: "Planned", category: "open" },
          { key: "approved", name: "Approved", category: "done", terminal: true },
        ],
        transitions: [{ key: "approve", name: "Approve", from: "planned", to: "approved" }],
      });
    });

    test.afterAll(async ({ request }) => {
      if (wfId) await request.delete(`/api/v1/admin/workflow-definitions/${wfId}`, { headers: { "X-CSRF-Token": await csrf(request) } });
    });

    test("defaults, the limit of 5, local checks, order and removing every step", async ({ page, request }) => {
      if (lang === "de") await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
      const stored = async () => {
        const d = await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`);
        return (d.transitions.find((t) => t.key === "approve")?.approval?.steps ?? []).map((s) => `${s.key}:${s.name}:${s.requiredApprovals}`);
      };

      await page.goto(`/admin/workflows/${wfId}?tab=designer`);
      await page.locator("table").getByRole("button", { name: "Approve", exact: true }).click();
      await expect(approval(page)).toContainText(m.none);

      // New steps: a default name in the page's language and a key no other step has.
      await approval(page).getByRole("button", { name: m.require }).click();
      await expect(step(page, 0).getByLabel(m.name)).toHaveValue(m.defaultName(1));
      await expect(step(page, 0).getByLabel(m.key)).toHaveValue("step_1");
      for (let n = 2; n <= 5; n++) await approval(page).getByRole("button", { name: m.addStep }).click();
      await expect(step(page, 4).getByLabel(m.key)).toHaveValue("step_5");
      await expect(approval(page).getByTestId("wf-add-step")).toBeDisabled();
      await expect(approval(page)).toContainText(m.max);
      await expect(saveState(page)).toHaveText(m.saved);
      expect(await stored()).toHaveLength(5);

      // Each local check is marked on its step, in the page's language, and holds back the save.
      const before = await stored();
      await step(page, 1).getByLabel(m.key).fill("step_1");
      await expect(step(page, 1).locator(".error")).toHaveText(m.duplicate("step_1"));
      await expect(step(page, 1).getByLabel(m.key)).toHaveAttribute("aria-invalid", "true");
      await expect(saveState(page)).toHaveText(m.fix);
      await step(page, 1).getByLabel(m.key).fill("cab");
      await step(page, 2).getByLabel(m.name).fill("   ");
      await expect(step(page, 2).locator(".error")).toHaveText(m.needsName(3));
      await expect(saveState(page)).toHaveText(m.fix);
      await step(page, 2).getByLabel(m.name).fill("Security");
      for (const n of ["21", "0"]) {
        await step(page, 3).getByLabel(m.required).fill(n);
        await expect(step(page, 3).locator(".error")).toHaveText(m.range(4));
        await expect(saveState(page)).toHaveText(m.fix);
      }
      expect(await stored(), "nothing invalid reaches the draft").toEqual(before);
      await step(page, 3).getByLabel(m.required).fill("20");
      await expect(saveState(page)).toHaveText(m.saved);
      expect(await stored()).toEqual([`step_1:${m.defaultName(1)}:1`, `cab:${m.defaultName(2)}:1`, `step_3:Security:1`, `step_4:${m.defaultName(4)}:20`, `step_5:${m.defaultName(5)}:1`]);

      // Moving step 1 down and removing the last two: saved in the new order, and a step can be added again.
      await step(page, 0).getByRole("button", { name: m.down(1) }).click();
      await step(page, 4).getByRole("button", { name: m.remove(5) }).click();
      await step(page, 3).getByRole("button", { name: m.remove(4) }).click();
      await expect(approval(page).getByTestId("wf-add-step")).toBeEnabled();
      await expect(saveState(page)).toHaveText(m.saved);
      expect(await stored()).toEqual([`cab:${m.defaultName(2)}:1`, `step_1:${m.defaultName(1)}:1`, `step_3:Security:1`]);

      // Removing every step saves the transition without approval (an empty policy would be refused).
      for (let j = 2; j >= 0; j--) await step(page, j).getByRole("button", { name: m.remove(j + 1) }).click();
      await expect(approval(page)).toContainText(m.none);
      await expect(approval(page).getByRole("button", { name: m.require })).toBeVisible();
      await expect(saveState(page)).toHaveText(m.saved);
      const d = await apiGet<Draft>(request, `/admin/workflow-definitions/${wfId}/draft`);
      expect(d.transitions.find((t) => t.key === "approve")?.approval ?? null).toBeNull();
    });
  });
}
