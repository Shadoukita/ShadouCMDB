import { apiGet, applySchemaChange, classIdByName, snap, expect, test } from "./support";

// Administration › Data model, Lookups and Templates, in order: a lookup list, a class
// built in the editor, a CI of that class, archiving, relationship rules and lookups.
// Every name carries a stamp, so the walk can run against a shared (demo) database.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const LIST = `E2E tier ${stamp}`;
const CLASS = `E2E appliance ${stamp}`;
const CLASS_KEY = `e2e_appliance_${stamp}`;
const CI = `e2e-appl-${stamp}`;
const REL = `E2E powers ${stamp}`;
let classId = "";

interface Page_<T> {
  data: T[];
}
interface Attr {
  key: string;
  label: string;
  dataType: string;
  isRequired: boolean;
  groupName: string | null;
  sortOrder: number;
  defaultValue: unknown;
  helpText: string | null;
  lookupListId: string | null;
  enumValues: string[] | null;
  validation: Record<string, unknown> | null;
}

test("the sub-navigation groups Access, Data model and System", async ({ page }) => {
  await page.goto("/admin/templates");
  const sub = page.getByRole("navigation", { name: "Administration" });
  await expect(sub.getByRole("heading")).toHaveText(["Access", "Data model", "System"]);
  await expect(sub.getByRole("link")).toHaveText(["Users", "Permission profiles", "API tokens", "Identity providers", "Areas", "CI classes", "Relationship types", "Dropdowns", "Lookups", "Templates", "Customization", "Export / import", "Audit log"]);
  await expect(sub.getByRole("link", { name: "Templates" })).toHaveAttribute("aria-current", "page");
});

test("templates: the installed starter cannot be installed twice", async ({ page }) => {
  await page.goto("/admin/templates");
  const panel = page.getByRole("region", { name: "IT infrastructure" });
  await expect(panel.locator(".badge", { hasText: "Installed" })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Installed" })).toBeDisabled();
  await expect(panel.getByRole("row", { name: /CI classes 8/ })).toBeVisible();
  await snap(page, "30-templates");
});

test("a lookup list with ordered values", async ({ page, request }) => {
  await page.goto("/admin/lookups");
  await expect(page).toHaveURL(/\/admin\/lookups\/statuses$/);
  // Lookup lists live under Data model › Dropdowns; the old Lookups › Lists address leads there.
  await expect(page.getByRole("navigation", { name: "Lookups" }).getByRole("link")).toHaveText(["Statuses", "Environments", "Locations", "Owners"]);
  await page.goto("/admin/lookups/lists");
  await expect(page).toHaveURL(/\/admin\/dropdowns$/);
  await expect(page.getByRole("heading", { level: 1, name: "Dropdowns" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Dropdowns" })).toHaveAttribute("aria-current", "page");
  await page.getByRole("button", { name: "+ New list" }).first().click();
  await page.locator("#ll-name").fill(LIST);
  await expect(page.locator("#ll-key")).toHaveValue(`e2e_tier_${stamp}`);
  await page.getByRole("button", { name: "Create list" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Created list ${LIST}.` })).toBeVisible();
  await expect(page.getByRole("heading", { name: `Values of “${LIST}”` })).toBeVisible();
  for (const v of ["Gold", "Silver", "Bronze"]) {
    await page.getByRole("button", { name: "+ Add value" }).first().click();
    await page.locator("#lookup-list-values-name").fill(v);
    await page.getByRole("button", { name: "Add value", exact: true }).click();
    await expect(page.getByRole("status").filter({ hasText: `Added value ${v}.` })).toBeVisible();
  }
  // The arrow buttons are the keyboard way to reorder.
  await page.getByRole("button", { name: "Move Bronze up" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Moved Bronze." })).toBeVisible();
  const values = page.getByRole("region", { name: `Values of “${LIST}”` });
  await expect(values.locator("tbody tr td:nth-child(2)")).toHaveText(["Gold", "Bronze", "Silver"]);

  const lists = await apiGet<Page_<{ id: string; name: string }>>(request, `/lookup-lists?q=${encodeURIComponent(LIST)}`);
  const stored = await apiGet<Page_<{ name: string }>>(request, `/lookup-list-values?listId=${lists.data[0].id}&sort=sortOrder`);
  expect(stored.data.map((v) => v.name)).toEqual(["Gold", "Bronze", "Silver"]);
  await snap(page, "31-lookup-list");
});

test("a class and its attributes are built in the editor", async ({ page, request }) => {
  await page.goto("/admin/classes");
  await expect(page.getByRole("region", { name: "CI classes" }).getByRole("link", { name: "Hardware", exact: true })).toBeVisible();
  await page.getByRole("link", { name: "+ New class" }).click();
  await page.locator("#class-name").fill(CLASS);
  await expect(page.locator("#class-key")).toHaveValue(CLASS_KEY);
  await page.locator("#class-parent").selectOption({ label: "Hardware" });
  await page.locator("#class-icon").selectOption({ label: "Device" });
  await page.locator("#class-color").fill("#aa3377");
  // The class's table goes into its parent's area; the preview shows the DDL before anything runs.
  await expect(page.locator("#class-key-hint")).toContainText(`infrastruktur.${CLASS_KEY}`);
  await page.getByRole("button", { name: "Create class" }).click();
  await applySchemaChange(page, "Create class", `CREATE TABLE "infrastruktur"."${CLASS_KEY}"`);
  await expect(page.getByRole("status").filter({ hasText: `Created class ${CLASS} (table infrastruktur.${CLASS_KEY}).` })).toBeVisible();
  classId = page.url().split("/").pop()!;
  const cls = await apiGet<{ key: string; icon: string; color: string; parentId: string }>(request, `/ci-classes/${classId}`);
  expect(cls).toMatchObject({ key: CLASS_KEY, icon: "device", color: "#aa3377", parentId: await classIdByName(request, "Hardware") });

  // Inherited attributes are listed read-only, linked to the class that defines them.
  const inherited = page.getByRole("region", { name: "Inherited attributes" });
  await expect(inherited.getByRole("row", { name: /Manufacturer/ }).getByRole("link", { name: "Hardware" })).toBeVisible();

  const add = async (fill: () => Promise<void>, label: string) => {
    await page.getByRole("button", { name: "+ Add attribute" }).click();
    await page.locator("#ad-label").fill(label);
    await fill();
    await page.getByRole("button", { name: "Preview and add…" }).click();
    await applySchemaChange(page, "Add attribute", "ADD COLUMN");
    await expect(page.getByRole("status").filter({ hasText: `Added attribute ${label}.` })).toBeVisible();
  };
  await add(async () => {
    await page.locator("#ad-type").selectOption({ label: "Whole number" });
    await page.locator("#ad-required").check();
    await page.locator("#ad-section").fill("Physical");
    await page.locator("#ad-min").fill("1");
    await page.locator("#ad-default").fill("2");
    await page.locator("#ad-help").fill("Height in rack units");
  }, "Rack units");
  await add(async () => {
    await page.locator("#ad-type").selectOption({ label: "Lookup list" });
    await page.locator("#ad-list").selectOption({ label: LIST });
    await page.locator("#ad-section").fill("Support");
    await page.locator("#ad-default").selectOption({ label: "Silver" });
  }, "Tier");
  await add(async () => {
    await page.locator("#ad-type").selectOption({ label: "Choice list" });
    await page.locator("#ad-enum").fill("active\npassive");
    await page.locator("#ad-section").fill("Physical");
  }, "Mode");
  // An API validation error lands next to its field, and nothing is saved.
  await page.getByRole("button", { name: "+ Add attribute" }).click();
  await page.locator("#ad-label").fill("Rack units");
  // The technical name is checked live: rack_units is taken in this class.
  await expect(page.locator("#ad-key-err")).toBeVisible();
  await page.getByRole("button", { name: "Preview and add…" }).click();
  await expect(page.locator("#ad-key-err")).toBeVisible();
  await expect(page.locator("dialog.schema-change[open]")).toHaveCount(0);
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();

  const table = page.locator("table.attributes");
  await expect(table.locator("tr.section-row th")).toHaveText(["Physical", "Support"]);
  // Drag Mode onto Rack units: it takes that place.
  const row = (label: string) => table.locator("tbody tr").filter({ has: page.getByRole("button", { name: label, exact: true }) });
  await row("Mode").dragTo(row("Rack units"));
  await expect(page.getByRole("status").filter({ hasText: "Moved Mode." })).toBeVisible();
  // The arrow at a section boundary moves Tier into the section above.
  await page.getByRole("button", { name: "Move Tier up" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Moved Tier to section “Physical”." })).toBeVisible();
  await expect(table.locator("tr.section-row th")).toHaveText(["Physical"]);
  await snap(page, "32-attribute-editor");

  const own = await apiGet<Page_<Attr>>(request, `/attribute-definitions?classId=${classId}&sort=sortOrder`);
  expect(own.data.map((a) => [a.label, a.groupName])).toEqual([
    ["Mode", "Physical"],
    ["Rack units", "Physical"],
    ["Tier", "Physical"],
  ]);
  const rackUnits = own.data.find((a) => a.key === "rack_units")!;
  expect(rackUnits).toMatchObject({ dataType: "integer", isRequired: true, defaultValue: 2, helpText: "Height in rack units", validation: { min: 1 } });
  expect(own.data.find((a) => a.key === "mode")!.enumValues).toEqual(["active", "passive"]);
});

test("the CI form and detail page follow the new definitions", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: new RegExp(`^${CLASS}`) })).toBeVisible();
  await page.goto(`/cis/new?classId=${classId}`);
  // New CIs start from the defaults; help text shows under the field.
  await expect(page.locator("#attr-rack_units")).toHaveValue("2");
  await expect(page.locator("#attr-rack_units-hint")).toContainText("Height in rack units");
  await expect(page.locator("#attr-tier option:checked")).toHaveText("Silver");
  await expect(page.locator("#attr-tier option")).toHaveText(["— not set —", "Gold", "Bronze", "Silver"]);
  // Name and status are attributes the class inherits from Hardware.
  await page.locator("#attr-name").fill(CI);
  await page.locator("#attr-status").selectOption({ label: "In service" });
  await page.locator("#attr-tier").selectOption({ label: "Gold" });
  await page.locator("#attr-mode").selectOption("passive");
  await snap(page, "33-ci-form-from-admin-model");
  await page.getByRole("button", { name: `Create ${CLASS}` }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(CI);
  const attrs = page.locator(".layout-panels");
  await expect(attrs).toContainText("Gold");
  await expect(attrs).toContainText("passive");
  await expect(attrs.locator("dt", { hasText: "Rack units" }).locator("+ dd")).toHaveText("2");
});

test("an archived class keeps its CIs but takes no new ones", async ({ page }) => {
  await page.goto(`/admin/classes/${classId}`);
  await page.getByRole("button", { name: "Archive", exact: true }).click();
  await applySchemaChange(page, "Archive class");
  await expect(page.getByRole("status").filter({ hasText: `Archived ${CLASS}` })).toBeVisible();
  await expect(page.getByText("This class is archived")).toBeVisible();
  // Purging drops the table with its CIs: the dialog shows the DDL and wants the technical name typed.
  await page.getByRole("button", { name: "Purge…" }).click();
  const dialog = page.locator("dialog.schema-change[open]");
  await expect(dialog.locator(".sc-ddl")).toContainText(`DROP TABLE "infrastruktur"."${CLASS_KEY}"`);
  const purge = dialog.getByRole("button", { name: "Purge class and its CIs" });
  await expect(purge).toBeDisabled();
  await dialog.getByLabel(/Type .* to confirm/).fill("wrong");
  await expect(purge).toBeDisabled();
  await snap(page, "34-class-purge-preview");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: new RegExp(`^${CLASS}`) })).toHaveCount(0);
  await page.goto(`/cis/new?classId=${classId}`);
  await expect(page.getByRole("alert")).toContainText(`${CLASS} is archived`);
  await expect(page.locator("#attr-name")).toHaveCount(0);

  await page.goto("/admin/classes");
  const list = page.getByRole("region", { name: "CI classes" });
  await expect(list.getByRole("link", { name: CLASS })).toHaveCount(0);
  await page.getByLabel(/Show archived classes/).check();
  await expect(page).toHaveURL(/archived=show/);
  await list.getByRole("row", { name: new RegExp(CLASS) }).getByRole("button", { name: "Restore" }).click();
  // Restoring runs no DDL, so it applies without a preview.
  await expect(page.getByRole("status").filter({ hasText: `Restored ${CLASS}` })).toBeVisible();
});

test("a relationship type with a rule", async ({ page, request }) => {
  await page.goto("/admin/relationships");
  await expect(page.getByRole("link", { name: "Runs on", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "+ New relationship type" }).click();
  await page.locator("#rt-name").fill(REL);
  await page.locator("#rt-forwardLabel").fill("powers");
  await page.locator("#rt-reverseLabel").fill("is powered by");
  await page.getByRole("button", { name: "Create relationship type" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Created relationship type ${REL}.` })).toBeVisible();
  await expect(page.getByRole("heading", { name: `Rules for “${REL}”` })).toBeVisible();
  await expect(page.getByText(`no two CIs can be related with “${REL}”`)).toBeVisible();

  await page.locator("#rule-source").selectOption({ label: CLASS });
  await page.locator("#rule-target").selectOption({ label: "Server" });
  await page.getByRole("button", { name: "Add rule" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Added rule: ${CLASS} powers Server.` })).toBeVisible();
  await expect(page.getByRole("cell", { name: `${CLASS} powers Server · Server is powered by ${CLASS}` })).toBeVisible();
  await snap(page, "35-relationship-rules");

  // The "add relationship" picker now offers the type between these classes.
  const serverId = await classIdByName(request, "Server");
  const legal = await apiGet<Page_<{ name: string }>>(request, `/relationship-types?sourceClassId=${classId}&targetClassId=${serverId}`);
  expect(legal.data.map((t) => t.name)).toContain(REL);
});

test("lookups: add, archive and delete; values in use cannot be deleted", async ({ page }) => {
  const ENV = `E2E env ${stamp}`;
  await page.goto("/admin/lookups/environments");
  await page.getByRole("button", { name: "+ Add environment" }).first().click();
  await page.locator("#environments-name").fill(ENV);
  await page.getByRole("button", { name: "Add environment", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Added environment ${ENV}.` })).toBeVisible();
  const row = page.getByRole("row", { name: new RegExp(ENV) });
  await row.getByRole("button", { name: "Archive" }).click();
  await expect(row.getByText("Archived", { exact: true })).toBeVisible();
  await row.getByRole("button", { name: `Delete environment “${ENV}”` }).click();
  await expect(page.getByRole("dialog")).toContainText("Nothing refers to it");
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Deleted environment ${ENV}.` })).toBeVisible();
  await expect(page.getByRole("row", { name: new RegExp(ENV) })).toHaveCount(0);

  // CIs hold their status as a value of the "status" lookup list, under Dropdowns.
  await page.goto("/admin/dropdowns");
  await page.getByRole("region", { name: "Lookup lists" }).getByRole("link", { name: "Status", exact: true }).click();
  await page.getByRole("button", { name: "Delete value “In service”" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("cannot be deleted while it is in use");
  await expect(dialog.getByRole("button", { name: "Archive instead" })).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();

  const TEAM = `E2E team ${stamp}`;
  await page.goto("/admin/lookups/owners");
  await page.getByRole("button", { name: "+ Add owner" }).first().click();
  await page.locator("#own-name").fill(TEAM);
  await page.getByRole("button", { name: "Add owner", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: `Added owner ${TEAM}.` })).toBeVisible();
  await page.locator("#own-q").fill(TEAM);
  await expect(page).toHaveURL(/\/admin\/lookups\/owners\?q=/);
  await expect(page.locator("tbody tr")).toHaveCount(1);
  await expect(page.getByRole("row", { name: new RegExp(TEAM) })).toContainText("Team");
});

test("a fresh install guides the administrator to the data model", async ({ page }) => {
  // What a bare install answers: no classes and no CIs.
  const empty = { data: [], page: { limit: 50, offset: 0, total: 0 } };
  await page.route("**/api/v1/ci-classes?*", (route) => route.fulfill({ json: empty }));
  await page.route("**/api/v1/configuration-items?*", (route) => route.fulfill({ json: empty }));
  for (const path of ["/", "/cis", "/cis/new"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { name: "No CI classes are defined yet" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Install a starter template" })).toHaveAttribute("href", "/admin/templates");
    await expect(page.getByRole("link", { name: "+ Create a class" })).toHaveAttribute("href", "/admin/classes/new");
  }
  await expect(page.getByRole("navigation", { name: "Main" }).getByText("No classes yet.")).toBeVisible();
  await snap(page, "36-fresh-install");
  await page.goto("/admin/classes");
  await expect(page.getByRole("heading", { name: "No CI classes yet" })).toBeVisible();
});
