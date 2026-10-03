import { readFileSync } from "node:fs";
import { apiGet, apiSend, expect, shownValue, snap, test } from "./support";

// An instance upgraded in place from a release before migration 0016 (barebone CI core), seen
// through the UI: every value the old fixed CI columns held (name, status, environment, owner,
// location, hostname, IP address, serial number, notes) is a field of the CI's class now, the CI
// is labelled by its former name, and the edit form saves the migrated values back unchanged.
// Reads the snapshot `tools/upgrade/upgrade-check.ts seed` took from the old release, so it checks
// the values that were really there. .github/workflows/upgrade.yml runs it after the upgrade:
//   E2E_BASE_URL=http://127.0.0.1:3000 E2E_USERNAME=upgrade-admin E2E_PASSWORD=... \
//   E2E_UPGRADE_SNAPSHOT=../snapshot.json npx playwright test e2e/core-upgrade.spec.ts
const snapshotPath = process.env.E2E_UPGRADE_SNAPSHOT;
test.skip(!snapshotPath, "E2E_UPGRADE_SNAPSHOT (the upgrade-check snapshot of an upgraded instance) is not set");
test.describe.configure({ mode: "serial" });

interface Named {
  id: string;
  name: string;
}
/** A CI as a release before migration 0016 returned it. */
interface OldCi {
  id: string;
  name?: string;
  classId: string;
  status: Named;
  environment: Named | null;
  owner: Named | null;
  location: Named | null;
  hostname: string | null;
  ipAddress: string | null;
  serialNumber: string | null;
  notes: string | null;
}
interface Snapshot {
  ids: {
    cis: Record<string, string>;
    deletedCi: string;
    classes: { server: string; application: string };
  };
  objects: Record<string, unknown>;
}
interface Ci {
  ident: string;
  label: string;
  version: number;
  attributes: Record<string, unknown>;
}

const snapshot: Snapshot = snapshotPath ? JSON.parse(readFileSync(snapshotPath, "utf8")) : { ids: { cis: {} }, objects: {} };
const oldCi = (id: string) => snapshot.objects[`/api/v1/configuration-items/${id}`] as OldCi;
const seeded = Object.values(snapshot.ids.cis).map(oldCi);
// A release that already had the barebone core returns a label, not a name: nothing for 0016 to move.
test.skip(!!snapshotPath && seeded.some((c) => c.name === undefined), "the old release already had the barebone CI core");

const server = () => oldCi(snapshot.ids.cis.server!);
const deleted = () => oldCi(snapshot.ids.deletedCi);

/** The former fixed fields of a CI with the label of the class field they became, as the detail page shows them. */
function formerFields(ci: OldCi): [label: string, text: string][] {
  const fields: [string, string | null | undefined][] = [
    ["Name", ci.name],
    ["Status", ci.status.name],
    ["Environment", ci.environment?.name],
    ["Owner", ci.owner?.name],
    ["Location", ci.location?.name],
    ["Hostname", ci.hostname],
    ["IP address", ci.ipAddress],
    ["Serial number", ci.serialNumber],
    ["Notes", ci.notes],
  ];
  return fields.filter((f): f is [string, string] => f[1] != null);
}

test('every former fixed field shows on the detail page as a class field; no "Other" section', async ({ page }) => {
  for (const ci of seeded) {
    await page.goto(`/cis/${ci.id}`);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(ci.name!);
    // A layout saved before the upgrade may put its own sections first.
    const sections = page.locator(".layout-container details > summary h2");
    await expect(sections.filter({ hasText: /^General$/ })).toHaveCount(1);
    await expect(sections.filter({ hasText: /^Other$/ })).toHaveCount(0);
    // The page opens with the values in their inputs (SHAA-1644); an empty valid until is open-ended.
    await expect.poll(() => shownValue(page, "Ident")).toMatch(/^CI-[0-9A-HJKMNP-TV-Z]{8}$/);
    await expect.poll(() => shownValue(page, "Valid until")).toBe("");
    await expect.poll(() => shownValue(page, "Active")).toBe("Active");
    for (const [label, text] of formerFields(ci)) {
      // Notes keep their line breaks; the page may lay them out on separate lines.
      await expect.poll(() => shownValue(page, label), { message: `${ci.name}: ${label}` }).toBe(text.replace(/\s+/g, " ").trim());
    }
  }
  await page.goto(`/cis/${server().id}`);
  await snap(page, "upgrade-detail");
});

test("the CI is labelled by its former name in the inventory, search and references", async ({ page }) => {
  await page.goto(`/cis?classId=${snapshot.ids.classes.server}`);
  const labels = page.locator("table tbody tr td:first-child");
  const live = seeded.filter((c) => c.classId === snapshot.ids.classes.server).map((c) => c.name!);
  // In any order: the administrator's list view that upgrade-check.ts seeds sorts the list by creation date.
  await expect(labels).toHaveCount(live.length);
  expect((await labels.allInnerTexts()).sort()).toEqual(live.sort());
  // The CI deleted before the upgrade stays deleted.
  await expect(page.getByRole("link", { name: deleted().name })).toHaveCount(0);

  await page.goto(`/cis?q=${encodeURIComponent(server().name!)}`);
  await expect(page.getByRole("link", { name: server().name, exact: true })).toBeVisible();

  // The application's relationship and the server's reference field name each other by label.
  const app = oldCi(snapshot.ids.cis.application!);
  await page.goto(`/cis/${app.id}`);
  await expect(page.locator("table").getByRole("link", { name: server().name, exact: true })).toBeVisible();
  await page.goto(`/cis/${server().id}`);
  await expect.poll(() => shownValue(page, "primary app")).toBe(app.name!);
});

test("the class's title attribute is the migrated Name field", async ({ page }) => {
  await page.goto(`/admin/classes/${snapshot.ids.classes.server}`);
  await expect(page.locator("#class-title").locator("option:checked")).toHaveText(/^Name\b/);
});

test("the edit form holds the migrated values and saves them back unchanged", async ({ page, request }) => {
  const ci = server();
  const before = await apiGet<Ci>(request, `/configuration-items/${ci.id}`);
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("#attr-name")).toHaveValue(ci.name!);
  if (ci.hostname) await expect(page.locator("#attr-hostname")).toHaveValue(ci.hostname);
  if (ci.ipAddress) await expect(page.locator("#attr-ip_address")).toHaveValue(ci.ipAddress);
  if (ci.serialNumber) await expect(page.locator("#attr-serial_number")).toHaveValue(ci.serialNumber);
  await expect(page.locator("#attr-status option:checked")).toHaveText(ci.status.name);
  if (ci.location) await expect(page.locator("#attr-location option:checked")).toHaveText(ci.location.name);
  await expect(page.locator("#f-ident")).toHaveValue(before.ident);
  await snap(page, "upgrade-edit");

  // One edit through the form: the migrated values must pass today's field validation as they are.
  await page.locator("#attr-serial_number").fill(`${ci.serialNumber ?? "SN"}-EDITED`);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  const after = await apiGet<Ci>(request, `/configuration-items/${ci.id}`);
  expect(after.version).toBe(before.version + 1);
  expect(after.attributes).toEqual({
    ...before.attributes,
    serial_number: `${ci.serialNumber ?? "SN"}-EDITED`,
  });
  expect({ ident: after.ident, label: after.label }).toEqual({
    ident: before.ident,
    label: before.label,
  });

  // Put it back, so a later check on this database sees the upgraded value.
  const restored: Ci = await apiSend(request, "PATCH", `/configuration-items/${ci.id}`, {
    version: after.version,
    attributes: { serial_number: ci.serialNumber },
  });
  expect(restored.attributes).toEqual(before.attributes);
});

test("multi-line notes keep their line breaks when edited in the form", async ({ page, request }) => {
  // The migrated Notes field is a multi-line text area, so an edit keeps the line breaks (GH#109).
  const ci = server();
  test.skip(!ci.notes?.includes("\n"), "the seeded CI has no multi-line notes");
  const before = await apiGet<Ci>(request, `/configuration-items/${ci.id}`);
  await page.goto(`/cis/${ci.id}/edit`);
  const notes = page.locator("#attr-notes");
  await expect(notes).toHaveJSProperty("tagName", "TEXTAREA");
  await notes.press("ControlOrMeta+End");
  await notes.pressSequentially(" Checked.");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  const saved = (await apiGet<Ci>(request, `/configuration-items/${ci.id}`)).attributes.notes;
  // Put the upgraded value back before judging, so the database stays as the upgrade left it.
  const current = await apiGet<Ci>(request, `/configuration-items/${ci.id}`);
  await apiSend(request, "PATCH", `/configuration-items/${ci.id}`, {
    version: current.version,
    attributes: { notes: before.attributes.notes },
  });
  expect(saved).toBe(`${ci.notes} Checked.`);
});
