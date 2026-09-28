import { apiGet, apiSend, applySchemaChange, classIdByName, createCi, snap, expect, test } from "./support";

// Multi-line text attributes (GH#109): a text attribute flagged validation.multiline is a text
// area on the CI form that keeps line breaks and indentation exactly, and the detail page shows
// them. The migrated and template Notes fields carry the flag; the class editor sets and clears it.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
// A leading indent, a blank line and a trailing line break: all of it must survive the round-trip.
const RUNBOOK = "  Restart procedure:\n  1. Drain the node\n  2. Restart the service\n\nEscalate to the on-call DBA.\n";

interface Ci {
  id: string;
  attributes: Record<string, unknown>;
}
interface Attr {
  id: string;
  key: string;
  validation: Record<string, unknown> | null;
}

test("Notes: entered with line breaks, saved and reloaded unchanged", async ({ page, request }) => {
  const ci = await createCi(request, await classIdByName(request, "Application"), `e2e-multiline-${stamp}`);
  await page.goto(`/cis/${ci.id}/edit`);
  const notes = page.locator("#attr-notes");
  await expect(notes).toHaveJSProperty("tagName", "TEXTAREA");
  await notes.fill(RUNBOOK);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));

  expect((await apiGet<Ci>(request, `/configuration-items/${ci.id}`)).attributes.notes).toBe(RUNBOOK);
  // The detail page keeps the line breaks.
  const shown = page.locator(".prop", { has: page.locator("dt", { hasText: /^Notes$/ }) }).locator(".multiline");
  await expect(shown).toHaveJSProperty("textContent", RUNBOOK);
  await expect(shown).toHaveCSS("white-space", "pre-wrap");
  await snap(page, "multiline-detail");

  // Reloaded into the form, and saved again with another field changed: the notes stay as they are.
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("#attr-notes")).toHaveValue(RUNBOOK);
  await page.locator("#attr-notes").press("ControlOrMeta+End");
  await page.locator("#attr-notes").pressSequentially("Checked.");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  expect((await apiGet<Ci>(request, `/configuration-items/${ci.id}`)).attributes.notes).toBe(`${RUNBOOK}Checked.`);
});

test("the class editor sets and clears Multiline, keeping the other text rules", async ({ page, request }) => {
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", {
    key: `e2e_runbook_${stamp}`,
    name: `E2E runbook ${stamp}`,
    parentId: await classIdByName(request, "Hardware"),
  });
  const runbook = async () =>
    (await apiGet<{ data: Attr[] }>(request, `/ci-classes/${cls.id}/attributes`)).data.find((a) => a.key === "runbook");

  await page.goto(`/admin/classes/${cls.id}`);
  await page.getByRole("button", { name: "+ Add attribute" }).click();
  await page.locator("#ad-label").fill("Runbook");
  await page.locator("#ad-type").selectOption({ label: "Text" });
  await page.locator("#ad-maxlength").fill("2000");
  await expect(page.locator("#ad-default")).toHaveJSProperty("tagName", "INPUT");
  await page.getByLabel("Text area that keeps line breaks").check();
  // The default value follows: a multi-line attribute gets a multi-line default.
  await expect(page.locator("#ad-default")).toHaveJSProperty("tagName", "TEXTAREA");
  await page.getByRole("button", { name: "Preview and add…" }).click();
  await applySchemaChange(page, "Add attribute", "ADD COLUMN");
  await expect(page.getByRole("status").filter({ hasText: "Added attribute Runbook." })).toBeVisible();
  expect((await runbook())?.validation).toEqual({ maxLength: 2000, multiline: true });

  // Cleared: the key goes, maxLength stays.
  await page.locator("table.attributes").getByRole("button", { name: "Runbook", exact: true }).click();
  await expect(page.getByLabel("Text area that keeps line breaks")).toBeChecked();
  await page.getByLabel("Text area that keeps line breaks").uncheck();
  await page.getByRole("button", { name: "Save attribute" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved attribute Runbook." })).toBeVisible();
  expect((await runbook())?.validation).toEqual({ maxLength: 2000 });
});
