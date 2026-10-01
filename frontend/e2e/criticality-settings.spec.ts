import type { Page } from "@playwright/test";
import { apiGet, classIdByName, expect, resetUiSettings, test } from "./support";

// Criticality as a stored list view column and sort, and as a layout field that can be
// placed, made read-only and hidden (SHAA-945). Against the demo seed's Server class; the
// settings apply to every user, so the walk starts from and ends with the built-in settings.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);

async function save(page: Page, comment: string) {
  const bar = page.getByRole("region", { name: "Save changes" });
  await bar.getByLabel("Comment for this version").fill(`${comment} ${stamp}`);
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: /Saved as version \d+/ })).toBeVisible();
  await expect(bar.getByText("No unsaved changes")).toBeVisible();
}

test.beforeAll(async ({ request }) => resetUiSettings(request));
test.afterAll(async ({ request }) => resetUiSettings(request));

test("list views: Criticality is a column and a default sort of a class's list", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/admin/customization/list-views?class=server");
  await page.getByRole("button", { name: "Customize the Server list" }).click();
  await page.getByLabel("Add to Columns").selectOption({ label: "Criticality" });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByLabel("Default sort").selectOption("criticality");
  await expect(page.getByLabel("List preview").getByRole("columnheader", { name: "Criticality" })).toBeVisible();
  await save(page, "e2e criticality list view");

  const stored = await apiGet<{ settings: { listViews: { classKey: string; columns?: string[]; defaultSort?: { field: string } }[] } }>(request, "/ui-settings");
  const view = stored.settings.listViews.find((v) => v.classKey === "server");
  expect(view?.columns).toContain("criticality");
  expect(view?.defaultSort?.field).toBe("criticality");

  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.getByRole("columnheader", { name: /Criticality/ })).toHaveAttribute("aria-sort", "ascending");
});

test("layouts: Criticality is placed like the other fields, can be read-only and hidden", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/admin/customization/layouts?class=server");
  await page.getByRole("button", { name: "Customize the Server layout" }).click();
  const frame = page.getByTestId("designer-frame");
  const chip = (label: string) => frame.locator(".designer-field").filter({ has: page.locator(".designer-label", { hasText: new RegExp(`^${label}\\*?$`) }) });
  const props = page.getByRole("complementary", { name: "Layout properties" });
  // The built-in General section holds it after the other core fields.
  await expect(frame.locator(".designer-section").first().locator(".designer-label").nth(3)).toHaveText("Criticality");

  await chip("Criticality").click();
  await expect(props.getByRole("button", { name: "Hide field" })).toBeEnabled();
  await props.getByLabel("Read-only on the form").check();
  await save(page, "e2e criticality read-only");
  await page.goto(`/cis/new?classId=${serverId}`);
  await expect(page.getByLabel("Criticality")).toBeDisabled();

  await page.goto("/admin/customization/layouts?class=server");
  await chip("Criticality").focus();
  await page.keyboard.press("Delete");
  await expect(page.getByTestId("designer-hidden").getByRole("listitem")).toHaveText([/Criticality/]);
  await save(page, "e2e criticality hidden");
  const stored = await apiGet<{ settings: { layouts: { classKey: string; hiddenFields?: string[] }[] } }>(request, "/ui-settings");
  expect(stored.settings.layouts.find((l) => l.classKey === "server")?.hiddenFields).toContain("criticality");

  await page.goto(`/cis/new?classId=${serverId}`);
  await expect(page.getByLabel("Ident")).toBeVisible();
  await expect(page.getByLabel("Criticality")).toHaveCount(0);
});
