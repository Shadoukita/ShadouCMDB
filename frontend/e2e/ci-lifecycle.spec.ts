import type { Page } from "@playwright/test";
import { apiGet, apiSend, ciIdByName, classIdByName, pickCi, snap, expect, test, withInventoryFilters } from "./support";

// One CI walks the whole lifecycle: create (with validation) → find in the inventory
// → edit on its page → version conflict → relate → navigate → delete.
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
  const sections = page.locator("form .layout-panel > .panel-header h2");
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
  // The page opens with the fields as inputs (SHAA-1644).
  const generalPanel = page.locator(".layout-panels > .layout-panel").first();
  await expect(generalPanel.locator(".panel-header h2")).toHaveText("General");
  await expect(generalPanel.locator(".field").first().locator("label, .label")).toHaveText("Ident");
  await expect(generalPanel.locator("#f-ident")).toHaveValue(/CI-/);
  await expect(generalPanel.locator("#attr-hostname")).toHaveValue(`${name}.example.internal`);
  await expect(page.locator(".layout-panels > .layout-panel > .panel-header h2").last()).toHaveText("Record");
  await snap(page, "05-created-detail");
});

test("inventory: search and class filter live in the URL and survive a reload", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/cis");
  const listRequest = page.waitForRequest((r) => r.url().includes("/api/v1/configuration-items?") && r.url().includes(`q=${name}`));
  await page.locator("#f-q").fill(name);
  const req = await listRequest; // filtering happens in the API, not the browser
  expect(new URL(req.url()).searchParams.get("limit")).toBe("50");
  await withInventoryFilters(page, () => page.locator("#f-class").selectOption({ label: "Server" }));
  await expect(page).toHaveURL(new RegExp(`q=${name}`));
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page.getByRole("link", { name, exact: true })).toBeVisible();

  await page.reload();
  // The query bar shows the class filter as a token ahead of the search term.
  await expect(page.locator("#f-q")).toHaveValue(new RegExp(`^class:\\S+ ${name}$`));
  await withInventoryFilters(page, () => expect(page.locator("#f-class")).toHaveValue(serverId));
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

test("edit: the CI opens editable; Save appears once something changed and the page stays as it is", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  // No separate edit mode: the values are inputs, and with nothing changed there is nothing to save.
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toHaveCount(0);
  await expect(page.locator("#attr-name")).toHaveValue(name);
  await expect(page.locator("#attr-cpu_cores")).toHaveValue("16");
  const bar = page.getByRole("region", { name: "Unsaved changes" });
  await expect(bar).toHaveCount(0);

  // Typing a value back to what it was leaves nothing to save.
  await page.locator("#attr-cpu_cores").fill("17");
  await expect(bar).toBeVisible();
  await page.locator("#attr-cpu_cores").fill("16");
  await expect(bar).toHaveCount(0);

  await page.locator("#attr-hostname").fill(`${name}-renamed.example.internal`);
  await page.locator("#attr-cpu_cores").fill("32");
  await expect(bar).toContainText("Unsaved changes");
  await snap(page, "07a-unsaved-changes");
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${name}.` })).toBeVisible();
  await expect(bar).toHaveCount(0);
  await expect(page).toHaveURL(`/cis/${ciId}`);
  await expect(page.locator("#attr-hostname")).toHaveValue(`${name}-renamed.example.internal`);
  await expect(page.locator("#attr-cpu_cores")).toHaveValue("32");
  // The saved values are what a reload shows.
  await page.reload();
  await expect(page.locator("#attr-cpu_cores")).toHaveValue("32");

  await page.getByRole("tab", { name: "History" }).click();
  const diff = page.locator("ul.diff").first();
  await expect(diff).toContainText("attributes.hostname");
  await expect(diff).toContainText(`${name}.example.internal`);
  await expect(diff).toContainText("attributes.cpu_cores");
  await expect(diff.locator("li", { hasText: "attributes.cpu_cores" }).locator("del")).toHaveText("16");
  await expect(diff.locator("li", { hasText: "attributes.cpu_cores" }).locator("ins")).toHaveText("32");
  await snap(page, "07-history-diff");

  // Source chips filter on the server: the edits above came through the UI, none through an API token.
  const sources = page.getByRole("group", { name: "Filter by source" });
  const events = page.locator("table.event-table tbody tr");
  await expect(events.first().locator(".event-source")).toHaveText("UI");
  const api = sources.getByRole("button", { name: "API", exact: true });
  await api.click();
  await expect(api).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("status").filter({ hasText: "No changes from the selected sources." })).toBeVisible();
  await sources.getByRole("button", { name: "UI", exact: true }).click();
  await expect(events.first().locator(".event-source")).toHaveText("UI");
  await sources.getByRole("button", { name: "Show all" }).click();
  await expect(api).toHaveAttribute("aria-pressed", "false");
  await expect(diff).toContainText("attributes.cpu_cores");
});

test("edit: Discard restores the values; leaving with unsaved changes asks first", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  const bar = page.getByRole("region", { name: "Unsaved changes" });
  await page.locator("#attr-serial_number").fill("SN-DISCARD");
  await bar.getByRole("button", { name: "Discard" }).click();
  await expect(bar).toHaveCount(0);
  await expect(page.locator("#attr-serial_number")).toHaveValue("");

  // A required field emptied is flagged before any request.
  await page.locator("#attr-name").fill("");
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.locator("#attr-name-err")).toHaveText("Required");
  await expect(page.locator("#attr-name")).toBeFocused();
  await page.locator("#attr-name").fill(name);
  await expect(bar).toHaveCount(0);

  // Another tab of the same CI keeps the changes; leaving the CI asks, and staying keeps them.
  await page.locator("#attr-serial_number").fill("SN-KEEP");
  await page.getByRole("tab", { name: "Relationship map" }).click();
  await page.getByRole("tab", { name: "Overview" }).click();
  await expect(page.locator("#attr-serial_number")).toHaveValue("SN-KEEP");
  page.once("dialog", (d) => {
    expect(d.message()).toBe(`Discard your unsaved changes to ${name}?`);
    void d.dismiss();
  });
  await page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Inventory" }).click();
  await expect(page).toHaveURL(`/cis/${ciId}`);
  await expect(page.locator("#attr-serial_number")).toHaveValue("SN-KEEP");
  page.once("dialog", (d) => void d.accept());
  await page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Inventory" }).click();
  await expect(page).toHaveURL(/\/cis(\?|$)/);
  await page.goto(`/cis/${ciId}`);
  await expect(page.locator("#attr-serial_number")).toHaveValue("");
});

test("edit: a concurrent change shows the 409 VERSION_CONFLICT banner", async ({ page, request }) => {
  await page.goto(`/cis/${ciId}`);
  await expect(page.locator("#attr-name")).toHaveValue(name);
  // Someone else saves first.
  const current = await apiGet<{ version: number }>(request, `/configuration-items/${ciId}`);
  await apiSend(request, "PATCH", `/configuration-items/${ciId}`, { attributes: { notes: "changed elsewhere" }, version: current.version });

  await page.locator("#attr-serial_number").fill("SN-CONFLICT");
  await page.getByRole("region", { name: "Unsaved changes" }).getByRole("button", { name: "Save" }).click();
  const banner = page.getByRole("alert").filter({ hasText: "Someone else saved this CI while you were editing." });
  await expect(banner).toBeVisible();
  await expect(banner).toContainText("Your changes were not saved.");
  await expect(page.locator("#attr-serial_number")).toHaveValue("SN-CONFLICT");
  await snap(page, "08-version-conflict");
  await banner.getByRole("button", { name: "Load the current version" }).click();
  await expect(banner).toHaveCount(0);
  await expect(page.locator("#attr-notes")).toHaveValue("changed elsewhere");
  await expect(page.locator("#attr-serial_number")).toHaveValue("");
  await expect(page.getByRole("region", { name: "Unsaved changes" })).toHaveCount(0);
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
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
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
  // The direction is an icon named for assistive technology (audit R5).
  await expect(panel.getByRole("row", { name: /is located in\s+FRA1 Rack A01/ }).getByRole("img", { name: "Outgoing" })).toBeVisible();
  await expect(panel.getByRole("row", { name: /hosts\s+CRM/ }).getByRole("img", { name: "Incoming" })).toBeVisible();

  await pickCi(page, "#rel-target", "Customer Relationship", "Customer Relationship Management");
  // A fresh install's template services are the built-in business service type (migration 0033).
  await expect(page.locator("#rel-type-hint")).toHaveText("No relationship rule allows Server ↔ Business service.");
  await expect(page.locator("#rel-type")).toBeDisabled();
  await snap(page, "09-relationships");
  await page.getByRole("button", { name: "Clear Customer Relationship Management" }).click();
  // Clearing the chosen CI brings the search box back with the focus in it (audit R10).
  await expect(page.locator("#rel-target")).toBeFocused();
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

test("relationship map: the topology canvas, 1 hop / 2 hops / Impact, and the tree as links", async ({ page, request }) => {
  const crm = await ciIdByName(request, "CRM");
  await page.goto(`/cis/${crm}`);
  await page.getByRole("tab", { name: "Relationship map" }).click();
  const panel = page.getByRole("tabpanel");
  const tree = panel.getByRole("tree", { name: "Relationship map" });
  // 1 hop, both directions: the canvas is one image with a summary, the tree the same CIs as links.
  await expect(panel.getByRole("radio", { name: "1 hop" })).toBeChecked();
  await expect(panel.getByRole("img", { name: /^CRM and \d+ related CIs within 1 hop\./ })).toBeVisible();
  await expect(tree.getByText("runs on", { exact: true }).first()).toBeVisible();
  await expect(tree.getByRole("link", { name: "crm-app-01", exact: true }).first()).toBeVisible();
  await expect(tree.getByRole("link", { name: "Customer Relationship Management", exact: true })).toBeVisible();
  await expect(tree.getByRole("link", { name: "fra1-esx-01", exact: true })).toHaveCount(0);
  // A canvas box is a link to its CI, for the mouse; the tree is the keyboard path.
  const box = panel.locator("a.topology-node").filter({ hasText: "crm-app-01" });
  await expect(box).toHaveAttribute("tabindex", "-1");
  await snap(page, "11-relationship-map");
  // Two hops: each row one hop further out. crm-db runs on crm-app-01 too, but both are one hop away, and
  // nothing leads back to CRM, so neither is a row.
  await panel.getByRole("radio", { name: "2 hops" }).check();
  await expect(tree.getByRole("link", { name: "fra1-esx-01", exact: true }).first()).toBeVisible();
  await expect(tree.getByRole("link", { name: "crm-app-01", exact: true })).toHaveCount(1);
  await expect(tree.getByRole("link", { name: "CRM", exact: true })).toHaveCount(0);
  await page.locator("#g-dir").selectOption("outgoing");
  await expect(tree.getByRole("link", { name: "Customer Relationship Management", exact: true })).toHaveCount(0);
  await expect(tree.getByRole("link", { name: "fra1-esx-01", exact: true }).first()).toBeVisible();
  // Impact: what CRM takes down, with the full analysis a link away.
  await panel.getByRole("radio", { name: "Impact" }).check();
  await expect(page.locator("#g-dir")).toHaveCount(0);
  await panel.getByRole("link", { name: "Open the impact analysis" }).first().click();
  await expect(page).toHaveURL(`/cis/${crm}/impact`);
  // The canvas box navigates like the tree's link.
  await page.getByRole("tab", { name: "Relationship map" }).click();
  await page.locator("a.topology-node").filter({ hasText: "crm-app-01" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("crm-app-01");
});

test("delete: the confirmation lists the relationships that will break", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
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
  await withInventoryFilters(page, () => page.locator("#f-deleted").selectOption("only"));
  await expect(page).toHaveURL(/deleted=only/);
  const row = page.getByRole("row", { name: new RegExp(name) });
  await expect(row).toContainText("Deleted");

  await page.goto(`/cis/${ciId}`);
  await expect(page.getByText(/This CI was deleted on/)).toBeVisible();
  // A deleted CI is shown read-only, in the same place: no inputs.
  await expect(page.locator("#attr-name")).toHaveCount(0);
  await expect(page.locator(".field-ro", { hasText: "Name" }).first()).toContainText(name);
});
