import { apiSend, checkA11y, chooseTheme, expect, test } from "./support";

// Administration › Import in the reference-mockup look (design document §0, step 9c-3): the CI page's head band
// without tabs (tile, status pill, Administrator only), the switch and the limits as panels, and the shared save
// bar docked at the bottom instead of a bare button in the panel (audit A3). Import is left off after each test.
test.describe.configure({ mode: "serial" });

test.afterEach(async ({ request }) => {
  await apiSend(request, "PUT", "/imports/settings", { enabled: false });
});

test("import settings: head band, panels and the shared save bar", async ({ page }, testInfo) => {
  await page.goto("/admin/import");
  const head = page.locator(".record-head-plain");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Import");
  await expect(head.getByRole("heading", { level: 1, name: "Import" })).toBeVisible();
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText("Turned off");
  await expect(meta).toContainText("Administrator only");

  await expect(page.getByRole("heading", { level: 2, name: "Bulk import" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "Limits" })).toBeVisible();
  await expect(page.getByText("Largest file")).toBeVisible();

  // Nothing changed: the save bar is there, Save is off and there is no Discard.
  const bar = page.getByRole("region", { name: "Save" });
  await expect(bar.getByRole("button", { name: "Save changes" })).toBeDisabled();
  await expect(bar.getByRole("button", { name: "Discard" })).toHaveCount(0);

  // A change shows "Unsaved changes"; Discard puts the stored value back.
  const toggle = page.getByLabel("Bulk import enabled");
  await toggle.check();
  await expect(bar).toContainText("Unsaved changes");
  await checkA11y(page, testInfo, "import-settings-dirty-light");
  await bar.getByRole("button", { name: "Discard" }).click();
  await expect(toggle).not.toBeChecked();
  await expect(bar).not.toContainText("Unsaved changes");

  // Save: a toast, and the pill follows the stored value.
  await toggle.check();
  await bar.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByText("Bulk import is turned on.")).toBeVisible();
  await expect(meta.locator(".badge").first()).toHaveText("Turned on");
  await expect(bar.getByRole("button", { name: "Save changes" })).toBeDisabled();

  await checkA11y(page, testInfo, "import-settings-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "import-settings-dark");
  await chooseTheme(page, "");
});

test("import settings: the page texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/import");
  const meta = page.locator(".record-head-plain").getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText("Ausgeschaltet");
  await expect(meta).toContainText("Nur Administrator");
  await expect(page.getByRole("heading", { level: 2, name: "Massenimport" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "Grenzwerte" })).toBeVisible();
  await expect(page.getByLabel("Massenimport eingeschaltet")).not.toBeChecked();
  await expect(page.locator(".panel").getByRole("link", { name: "Berechtigungsprofile" })).toHaveAttribute("href", "/admin/profiles");
  await expect(page.getByRole("button", { name: "Änderungen speichern" })).toBeDisabled();
});
