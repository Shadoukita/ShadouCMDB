import type { Page } from "@playwright/test";
import { apiGet, apiSend, ciIdByName, classIdByName, pickCi, snap, expect, test } from "./support";

// One CI walks the whole lifecycle: create (with validation) → find in the inventory
// → edit → version conflict → relate → navigate → delete.
test.describe.configure({ mode: "serial" });

const stamp = Date.now();
const name = `web-e2e-${stamp}`;
let ciId = "";

const submit = (page: Page, label: string) => page.getByRole("button", { name: label }).click();

test("create a Server from its class attributes, with field-level validation", async ({ page }) => {
  await page.goto("/cis/new");
  await expect(page.locator("#ci-class")).toBeFocused();
  await page.locator("#ci-class").selectOption({ label: "Server" });
  await expect(page).toHaveURL(/\/cis\/new\?classId=/);

  // General comes first: ident, validity, then the attributes without a group (name, hostname…).
  // The attribute groups follow as sections, from the API (groupName, in sortOrder).
  const sections = page.locator("form .layout-panel > summary h2");
  await expect(sections.first()).toHaveText("General");
  expect(await sections.allTextContents()).toEqual(expect.arrayContaining(["Hardware", "Compute", "Software", "Network"]));
  expect(await sections.allTextContents()).not.toContain("Other");
  const general = page.locator("form .layout-panel").first();
  await expect(general.locator("label").first()).toHaveText("Ident");
  await expect(general.locator("#attr-name")).toBeVisible();
  await expect(general.locator("#attr-hostname")).toBeVisible();
  await expect(page.locator("#attr-cpu_cores")).toHaveAttribute("type", "number");
  await expect(page.locator("#attr-purchase_date")).toHaveAttribute("type", "date");
  await expect(page.locator("#attr-os_family")).toHaveJSProperty("tagName", "SELECT");

  // Name and status are required attributes of the template's classes, flagged before any request.
  await submit(page, "Create Server");
  await expect(page.locator("#attr-name-err")).toHaveText("Required");
  await expect(page.locator("#attr-status-err")).toHaveText("Required");
  await expect(page.locator("#attr-name")).toBeFocused();
  // The ident is generated; only administrators may type one (the e2e user is one).
  await expect(page.locator("#f-ident")).toBeEnabled();
  await expect(page.locator("#f-valid-from")).toHaveAttribute("type", "datetime-local");

  // API validation errors land next to their field.
  await page.locator("#attr-name").fill(name);
  await page.locator("#attr-status").selectOption({ label: "In service" });
  await page.locator("#attr-hostname").fill("bad host!");
  await submit(page, "Create Server");
  await expect(page.locator("#attr-hostname-err")).toBeVisible();
  await expect(page.locator("#attr-hostname")).toHaveAttribute("aria-invalid", "true");
  await expect(page.getByRole("alert").first()).toContainText("Not saved");
  await snap(page, "03-create-validation-core");

  await page.locator("#attr-hostname").fill(`${name}.example.internal`);
  await page.locator("#attr-ip_address").fill("10.99.0.10");
  await page.locator("#attr-cpu_cores").fill("0");
  await submit(page, "Create Server");
  await expect(page.locator("#attr-cpu_cores-err")).toBeVisible();
  await snap(page, "04-create-validation-attribute");

  await page.locator("#attr-cpu_cores").fill("16");
  await page.locator("#attr-os_family").selectOption({ index: 1 });
  await submit(page, "Create Server");
  await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(name);
  await expect(page.getByRole("status").filter({ hasText: `Created ${name}.` })).toBeVisible();
  ciId = page.url().split("/").pop()!;
  // General panel first: ident, validity and the ungrouped attributes; class and timestamps last.
  const generalPanel = page.locator(".layout-panels > details").first();
  await expect(generalPanel.locator("summary h2")).toHaveText("General");
  await expect(generalPanel.locator("dt").first()).toHaveText("Ident");
  await expect(generalPanel).toContainText(/CI-/);
  await expect(generalPanel).toContainText(`${name}.example.internal`);
  await expect(page.locator(".layout-panels > details > summary h2").last()).toHaveText("Record");
  await snap(page, "05-created-detail");
});

test("inventory: search and class filter live in the URL and survive a reload", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/cis");
  const listRequest = page.waitForRequest((r) => r.url().includes("/api/v1/configuration-items?") && r.url().includes(`q=${name}`));
  await page.locator("#f-q").fill(name);
  const req = await listRequest; // filtering happens in the API, not the browser
  expect(new URL(req.url()).searchParams.get("limit")).toBe("50");
  await page.locator("#f-class").selectOption({ label: "Server" });
  await expect(page).toHaveURL(new RegExp(`q=${name}`));
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page.getByRole("link", { name, exact: true })).toBeVisible();

  await page.reload();
  await expect(page.locator("#f-q")).toHaveValue(name);
  await expect(page.locator("#f-class")).toHaveValue(serverId);
  await expect(page.getByRole("link", { name, exact: true })).toBeVisible();
  await snap(page, "06-inventory-filtered");

  // Global search results count their hits in the singular for one match (GH#46).
  await page.goto(`/search?q=${name}`);
  await expect(page.locator(".page-header")).toContainText("1 match");
  await expect(page.locator(".page-header")).not.toContainText("matches");

  // Sort and page size are URL state too, and paging is driven by page.total.
  await page.goto("/cis?limit=25&sort=-updatedAt");
  await expect(page.getByRole("columnheader", { name: /Updated/ })).toHaveAttribute("aria-sort", "descending");
  await expect(page.locator(".pagination")).toContainText(/1–\d+ of \d+/);
  await expect(page.locator(".pagination select")).toHaveValue("25");
  await page.getByRole("button", { name: /^Label/ }).click();
  await expect(page).toHaveURL(/sort=label/);
  await expect(page.getByRole("columnheader", { name: /^Label/ })).toHaveAttribute("aria-sort", "ascending");
});

test("edit: changes are saved with the version and shown in History", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("link", { name: "Edit", exact: true }).click();
  await expect(page).toHaveURL(`/cis/${ciId}/edit`);
  await expect(page.locator("#attr-name")).toHaveValue(name);
  await expect(page.locator("#attr-cpu_cores")).toHaveValue("16");
  await page.locator("#attr-hostname").fill(`${name}-renamed.example.internal`);
  await page.locator("#attr-cpu_cores").fill("32");
  await submit(page, "Save changes");
  await expect(page).toHaveURL(`/cis/${ciId}`);
  await expect(page.getByRole("status").filter({ hasText: `Saved ${name}.` })).toBeVisible();
  await expect(page.locator(".layout-panels")).toContainText(`${name}-renamed.example.internal`);
  await expect(page.locator(".layout-panels")).toContainText("32");

  await page.getByRole("tab", { name: "History" }).click();
  const diff = page.locator("ul.diff").first();
  await expect(diff).toContainText("attributes.hostname");
  await expect(diff).toContainText(`${name}.example.internal`);
  await expect(diff).toContainText("attributes.cpu_cores");
  await expect(diff.locator("li", { hasText: "attributes.cpu_cores" }).locator("del")).toHaveText("16");
  await expect(diff.locator("li", { hasText: "attributes.cpu_cores" }).locator("ins")).toHaveText("32");
  await snap(page, "07-history-diff");
});

test("edit: a concurrent change shows the 409 VERSION_CONFLICT banner", async ({ page, request }) => {
  await page.goto(`/cis/${ciId}/edit`);
  await expect(page.locator("#attr-name")).toHaveValue(name);
  // Someone else saves first.
  const current = await apiGet<{ version: number }>(request, `/configuration-items/${ciId}`);
  await apiSend(request, "PATCH", `/configuration-items/${ciId}`, { attributes: { notes: "changed elsewhere" }, version: current.version });

  await page.locator("#attr-serial_number").fill("SN-CONFLICT");
  await submit(page, "Save changes");
  const banner = page.getByRole("alert").filter({ hasText: "Someone else saved this CI while you were editing." });
  await expect(banner).toBeVisible();
  await expect(banner).toContainText("Your changes were not saved.");
  await snap(page, "08-version-conflict");
  await banner.getByRole("link", { name: "Open the current version" }).click();
  await expect(page).toHaveURL(`/cis/${ciId}`);
  await expect(page.locator(".layout-panels")).toContainText("changed elsewhere");
});

test("relationships: add in both directions; illegal pairs offer no type", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  const panel = page.getByRole("region", { name: "Relationships" });
  await expect(panel.getByRole("heading", { name: "No relationships yet" })).toBeVisible();

  await pickCi(page, "#rel-target", "FRA1 Rack", "FRA1 Rack A01");
  await page.locator("#rel-type").selectOption({ label: `${name} is located in FRA1 Rack A01` });
  await page.getByRole("button", { name: "Add relationship" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Added: ${name} is located in FRA1 Rack A01` })).toBeVisible();

  // With one relationship the delete confirmation speaks in the singular (GH#46); cancel, nothing is deleted.
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("This relationship will break:");
  await expect(dialog.getByRole("button", { name: "Delete CI and 1 relationship" })).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();

  // Reverse direction: the application runs on this server, so from here it reads "hosts".
  await pickCi(page, "#rel-target", "CRM", "CRM");
  await page.locator("#rel-type").selectOption({ label: `${name} hosts CRM` });
  await page.getByRole("button", { name: "Add relationship" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Added: ${name} hosts CRM` })).toBeVisible();

  await expect(panel.getByRole("row")).toHaveCount(3); // header + 2
  await expect(panel.getByRole("row", { name: /is located in\s+FRA1 Rack A01/ })).toBeVisible();
  await expect(panel.getByRole("row", { name: /hosts\s+CRM/ })).toContainText("← incoming");

  await pickCi(page, "#rel-target", "Customer Relationship", "Customer Relationship Management");
  await expect(page.locator("#rel-type-hint")).toHaveText("No relationship rule allows Server ↔ Service.");
  await expect(page.locator("#rel-type")).toBeDisabled();
  await snap(page, "09-relationships");
  await page.getByRole("button", { name: "Clear Customer Relationship Management" }).click();
});

test("navigation: related CIs are links and the breadcrumb carries the walk trail", async ({ page }) => {
  await page.goto("/cis?q=CRM");
  await page.getByRole("link", { name: "CRM", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("CRM");

  const rel = page.getByRole("region", { name: "Relationships" });
  await rel.getByRole("link", { name: "crm-app-01", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("crm-app-01");
  await rel.getByRole("link", { name: "fra1-esx-01", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("fra1-esx-01");
  await rel.getByRole("link", { name: "FRA1 Rack A01", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("FRA1 Rack A01");

  const crumbs = page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("listitem");
  const trail = ["Dashboard", "Inventory", "CRM", "crm-app-01", "fra1-esx-01", "FRA1 Rack A01"];
  await expect(crumbs).toHaveText(trail);
  await snap(page, "10-walk-breadcrumb");

  // The trail lives in history state: it survives a reload…
  await page.reload();
  await expect(crumbs).toHaveText(trail);
  // …and clicking a crumb walks back up.
  await crumbs.getByRole("link", { name: "crm-app-01" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("crm-app-01");
  await expect(crumbs).toHaveText(["Dashboard", "Inventory", "CRM", "crm-app-01"]);

  // Server → Application → Database
  await page.goto(`/cis/${ciId}`);
  await rel.getByRole("link", { name: "CRM", exact: true }).click();
  await rel.getByRole("link", { name: "crm-db", exact: true }).click();
  await expect(crumbs).toHaveText(["Dashboard", "Inventory", name, "CRM", "crm-db"]);

  // Device → Location
  await page.goto("/cis?q=fra1-tor-a01");
  await page.getByRole("link", { name: "fra1-tor-a01", exact: true }).click();
  await rel.getByRole("link", { name: "FRA1 Rack A01", exact: true }).click();
  await expect(crumbs).toHaveText(["Dashboard", "Inventory", "fra1-tor-a01", "FRA1 Rack A01"]);
});

test("relationship map renders the multi-hop tree as links", async ({ page, request }) => {
  const crm = await ciIdByName(request, "CRM");
  await page.goto(`/cis/${crm}`);
  await page.getByRole("tab", { name: "Relationship map" }).click();
  const panel = page.getByRole("tabpanel");
  await expect(panel.getByText("runs on →").first()).toBeVisible();
  // crm-app-01 is reached twice (CRM runs on it; crm-db, which CRM depends on, runs on it too).
  await expect(panel.getByRole("link", { name: "crm-app-01", exact: true }).first()).toBeVisible();
  await expect(panel.getByText("(shown above)").first()).toBeVisible();
  await expect(panel.getByRole("link", { name: "fra1-esx-01", exact: true }).first()).toBeVisible(); // two hops
  await page.locator("#g-depth").selectOption("1");
  await expect(panel.getByRole("link", { name: "fra1-esx-01", exact: true })).toHaveCount(0);
  await page.locator("#g-dir").selectOption("both");
  await expect(panel.getByRole("link", { name: "Customer Relationship Management", exact: true })).toBeVisible();
  await snap(page, "11-relationship-map");
});

test("delete: the confirmation lists the relationships that will break", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(`Delete server “${name}”?`);
  await expect(dialog).toContainText("These 2 relationships will break:");
  await expect(dialog.getByRole("listitem").filter({ hasText: "is located in FRA1 Rack A01 (Location)" })).toBeVisible();
  await expect(dialog.getByRole("listitem").filter({ hasText: "hosts CRM (Application)" })).toBeVisible();
  await snap(page, "12-delete-confirm");
  await dialog.getByRole("button", { name: "Delete CI and 2 relationships" }).click();
  await expect(page).toHaveURL(/\/cis$/);

  await page.goto(`/cis?q=${name}`);
  await expect(page.getByRole("heading", { name: "No configuration items match these filters" })).toBeVisible();
  await page.locator("#f-deleted").selectOption("only");
  await expect(page).toHaveURL(/deleted=only/);
  const row = page.getByRole("row", { name: new RegExp(name) });
  await expect(row).toContainText("Deleted");

  await page.goto(`/cis/${ciId}`);
  await expect(page.getByText(/This CI was deleted on/)).toBeVisible();
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toHaveCount(0);
});
