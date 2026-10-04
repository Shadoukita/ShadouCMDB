import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import { apiGet, csrf, expect, snap, test } from "./support";

// An instance upgraded in place from an older release, seen through the UI (saved-views spec §5.3 item 11):
// the inventory still opens with the administrator's list view that `tools/upgrade/upgrade-check.ts seed`
// set on the old release, and a view saved from it works. Its own file, not core-upgrade.spec.ts: that spec
// skips itself for releases that already had the barebone CI core, and this one applies to every release.
// .github/workflows/upgrade.yml runs it after the upgrade:
//   E2E_BASE_URL=http://127.0.0.1:3000 E2E_USERNAME=upgrade-admin E2E_PASSWORD=... \
//   E2E_UPGRADE_SNAPSHOT=../snapshot.json npx playwright test e2e/upgrade-saved-views.spec.ts
const snapshotPath = process.env.E2E_UPGRADE_SNAPSHOT;
test.skip(!snapshotPath, "E2E_UPGRADE_SNAPSHOT (the upgrade-check snapshot of an upgraded instance) is not set");
test.describe.configure({ mode: "serial" });

interface ListView {
  classKey: string;
  columns: string[];
  defaultSort: { field: string; direction: "asc" | "desc" };
  pageSize: number;
}
interface Snapshot {
  ids: { classes: { server: string }; listView?: ListView };
}
interface View {
  id: string;
  name: string;
  version: number;
  definition: { classKeys?: string[]; sort?: { field: string; direction: string }; columns?: string[]; pageSize?: number };
}

const snapshot: Snapshot = snapshotPath ? JSON.parse(readFileSync(snapshotPath, "utf8")) : { ids: { classes: { server: "" } } };
// Snapshots taken before the list view was seeded have nothing to check here.
test.skip(!!snapshotPath && !snapshot.ids.listView, "the snapshot has no administrator list view (taken by an older upgrade-check.ts)");
const serverId = snapshot.ids.classes.server;
const listView = snapshot.ids.listView!;
const NAME = "Upgrade check: servers by update";

const viewButton = (page: Page) => page.getByRole("button", { name: /^View / });
const menu = (page: Page) => page.getByRole("menu", { name: "Views" });
/** Column headers by their text (not innerText: the stylesheet sets headers in uppercase). */
const headers = async (page: Page) => (await page.locator("table.data thead th:not(.row-actions)").allTextContents()).map((h) => h.trim()).filter(Boolean);
const HEADER: Record<string, string> = { label: "Label", class: "Class", createdAt: "Created", updatedAt: "Updated" };
const SORT_HEADER = () => new RegExp(`^${HEADER[listView.defaultSort.field]}`);

test("the upgraded inventory opens with the administrator's list view, not a saved view", async ({ page }) => {
  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await expect(viewButton(page)).toContainText("Unsaved view");
  await expect(page).not.toHaveURL(/view=/);
  expect(await headers(page)).toEqual(["Label", ...listView.columns.map((c) => HEADER[c] ?? c)]);
  await expect(page.getByRole("columnheader", { name: SORT_HEADER() })).toHaveAttribute(
    "aria-sort",
    listView.defaultSort.direction === "desc" ? "descending" : "ascending",
  );
  await expect(page.getByLabel("Rows")).toHaveValue(String(listView.pageSize));
  await snap(page, "upgrade-list-view");
});

test("a view saved from the upgraded inventory keeps the list view's columns and comes back by link", async ({ page, request }) => {
  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  const rows = await page.locator("table.data tbody tr td:first-child").allInnerTexts();

  await viewButton(page).click();
  await menu(page).getByRole("menuitem", { name: "Save as new view…", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await dialog.getByLabel("Name").fill(NAME);
  await dialog.getByRole("button", { name: "Save view" }).click();
  await expect(dialog).toBeHidden();
  await expect(page).toHaveURL(/[?&]view=/);
  await expect(viewButton(page)).toContainText(NAME);

  const saved = (await apiGet<{ data: View[] }>(request, "/saved-views?context=inventory")).data.find((v) => v.name === NAME);
  expect(saved, "the view in GET /saved-views").toBeTruthy();
  expect(saved!.definition.classKeys).toEqual([listView.classKey]);
  expect(saved!.definition.sort).toEqual(listView.defaultSort);
  expect(saved!.definition.pageSize).toBe(listView.pageSize);

  // Opened from a link with nothing but view=<id>: the same rows and columns.
  const other = await page.context().newPage();
  await other.goto(`/cis?view=${saved!.id}`);
  await expect(viewButton(other)).toContainText(NAME);
  await expect(other.locator("table.data tbody tr").first()).toBeVisible();
  expect(await other.locator("table.data tbody tr td:first-child").allInnerTexts()).toEqual(rows);
  expect(await headers(other)).toEqual(await headers(page));
  await other.close();

  // Leave the upgraded database as the upgrade left it.
  const res = await request.delete(`/api/v1/saved-views/${saved!.id}?version=${saved!.version}`, { headers: { "X-CSRF-Token": await csrf(request) } });
  expect(res.status()).toBe(204);
});
