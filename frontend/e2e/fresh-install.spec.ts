import type { APIRequestContext } from "@playwright/test";
import { BARE_STATE, FRESH_ADMIN, IMPORT_TARGET_STATE } from "./global-setup";
import { apiGet, apiSend, applySchemaChange, expect, snap, test } from "./support";

// A bare install built up from nothing, then exported and imported into a second fresh install, nothing mocked.
// Two more APIs next to the shared demo instance, each on its own migrated and `seed`ed database (system rows only,
// no user; global setup creates FRESH_ADMIN on both). E2E_BARE_BASE_URL is built up here: starter template, an own
// class with a lookup list, CIs, customization, a profile. E2E_IMPORT_BASE_URL receives its export. CI starts both.
// A run consumes them, so point both at newly created databases for every run.
const bareURL = process.env.E2E_BARE_BASE_URL;
const targetURL = process.env.E2E_IMPORT_BASE_URL;
test.skip(!bareURL || !targetURL, "E2E_BARE_BASE_URL and E2E_IMPORT_BASE_URL (APIs on empty, seeded databases) are not set");
test.describe.configure({ mode: "serial" });

const CLASS = "Rack PDU";
const CLASS_KEY = "rack_pdu";
const LIST = "Support tier";
const APP = "Fresh CMDB";
const PROFILE = "PDU operators";
const CIS = [
  { name: "pdu-fra1-a01", tier: "Gold", outlets: "24" },
  { name: "pdu-fra1-a02", tier: "Silver", outlets: "16" },
];
/** The source's export, taken at the end of its build-up. */
let exported: ConfigFile;

interface Page_<T> {
  data: T[];
  page: { total: number };
}
interface ConfigFile {
  format: string;
  exportedAt: string;
  appVersion: string;
  dataModel: { classes: { key: string }[]; attributes: { class: string; key: string }[]; relationshipRules: unknown[] };
  lookups: { lists: { key: string; values: { key: string }[] }[] };
  permissionProfiles: { name: string }[];
  uiSettings: { settings: { branding: { appName: string | null } } };
}

async function exportConfig(request: APIRequestContext): Promise<ConfigFile> {
  const res = await request.get("/api/v1/admin/config/export");
  expect(res.ok(), `export → ${res.status()}`).toBeTruthy();
  expect(res.headers()["content-disposition"]).toContain("attachment");
  return (await res.json()) as ConfigFile;
}

/**
 * What must survive the round trip: everything but when and by which build the file was written, in the same order
 * (the export sorts every section by stable keys).
 */
const comparable = ({ exportedAt: _at, appVersion: _v, ...rest }: ConfigFile) => rest;

test.describe("a bare install", () => {
  test.use({ baseURL: bareURL, storageState: BARE_STATE });

  test("starts without a data model, and the starter template fills it in one click", async ({ page, request }) => {
    // What `seed` leaves: system rows only.
    for (const path of ["/statuses", "/environments", "/locations", "/owners", "/configuration-items"]) {
      expect((await apiGet<Page_<unknown>>(request, path)).page.total, path).toBe(0);
    }
    // The one lookup list is the system list behind the core Criticality field.
    const lists = await apiGet<Page_<{ key: string; systemRole: string | null }>>(request, "/lookup-lists");
    expect(lists.data.map((l) => [l.key, l.systemRole])).toEqual([["criticality", "criticality"]]);
    // The one class and the one relationship type are the built-in business service type and its member type (migration 0033).
    expect((await apiGet<Page_<{ key: string }>>(request, "/ci-classes")).data.map((c) => c.key)).toEqual(["business_service"]);
    expect((await apiGet<Page_<{ key: string }>>(request, "/relationship-types")).data.map((t) => t.key)).toEqual(["business_service_member"]);
    // The dashboard's data-model guide leaves out the built-in class once class reads carry systemRole (SHAA-927 §4); until then it
    // offers the first CI, so the templates page is opened directly.
    await page.goto("/");
    await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Business service/ })).toBeVisible();
    await snap(page, "50-bare-install");
    await page.goto("/admin/templates");
    await expect(page.getByRole("heading", { name: "Your CMDB is empty" })).toBeVisible();

    const panel = page.getByRole("region", { name: "IT infrastructure" });
    await expect(panel.locator(".badge", { hasText: "Not installed" })).toBeVisible();
    await panel.getByRole("button", { name: "Install IT infrastructure starter" }).click();
    await expect(panel.getByRole("status")).toContainText("Installed IT infrastructure.");
    // The area, 8 classes, 69 attributes, 4 relationship types, 12 rules, 4 lookup lists and their 17 values, less the
    // service class and its name field: the built-in business service type stands in for them.
    await expect(panel.getByRole("status")).toContainText("Added 113 rows");
    await expect(panel.getByRole("status")).toContainText('CREATE SCHEMA "infrastruktur"');
    await expect(panel.locator(".badge", { hasText: "Installed" })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Installed" })).toBeDisabled();
    await expect(page.getByRole("heading", { name: "Your CMDB is empty" })).toHaveCount(0);
    await snap(page, "51-template-installed");

    // Persisted, and a second install is a no-op.
    const templates = await apiGet<{ data: { key: string; status: string }[] }>(request, "/admin/templates");
    expect(templates.data.find((t) => t.key === "it_infrastructure")?.status).toBe("installed");
    expect((await apiGet<Page_<unknown>>(request, "/ci-classes")).page.total).toBe(8);
    const again = await apiSend<{ created: Record<string, number> }>(request, "POST", "/admin/templates/it_infrastructure/install", {});
    expect(Object.values(again.created).every((n) => n === 0), JSON.stringify(again.created)).toBeTruthy();

    // The template's classes are ready for CIs.
    await panel.getByRole("link", { name: "Create the first CI" }).click();
    await expect(page.getByRole("heading", { name: "No CI classes are defined yet" })).toHaveCount(0);
    await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Server/ })).toBeVisible();
  });

  test("a class with a lookup-list attribute is built and used in the inventory", async ({ page, request }) => {
    // The lookup list the class refers to.
    await page.goto("/admin/dropdowns");
    await page.getByRole("button", { name: "+ New list" }).first().click();
    await page.locator("#ll-name").fill(LIST);
    await page.getByRole("button", { name: "Create list" }).click();
    await expect(page.getByRole("status").filter({ hasText: `Created list ${LIST}.` })).toBeVisible();
    for (const v of ["Gold", "Silver"]) {
      await page.getByRole("button", { name: "+ Add value" }).first().click();
      await page.locator("#lookup-list-values-name").fill(v);
      await page.getByRole("button", { name: "Add value", exact: true }).click();
      await expect(page.getByRole("status").filter({ hasText: `Added value ${v}.` })).toBeVisible();
    }

    await page.goto("/admin/classes/new");
    await page.locator("#class-name").fill(CLASS);
    await expect(page.locator("#class-key")).toHaveValue(CLASS_KEY);
    await page.locator("#class-parent").selectOption({ label: "Hardware" });
    await page.getByRole("button", { name: "Create class" }).click();
    await applySchemaChange(page, "Create class", "CREATE TABLE");
    await expect(page.getByRole("status").filter({ hasText: `Created class ${CLASS} (table` })).toBeVisible();
    const classId = page.url().split("/").pop()!;
    const add = async (label: string, fill: () => Promise<void>) => {
      await page.getByRole("button", { name: "+ Add attribute" }).click();
      await page.locator("#ad-label").fill(label);
      await fill();
      await page.getByRole("button", { name: "Preview and add…" }).click();
      await applySchemaChange(page, "Add attribute", "ADD COLUMN");
      await expect(page.getByRole("status").filter({ hasText: `Added attribute ${label}.` })).toBeVisible();
    };
    await add("Outlets", async () => {
      await page.locator("#ad-type").selectOption({ label: "Whole number" });
      await page.locator("#ad-required").check();
    });
    await add("Tier", async () => {
      await page.locator("#ad-type").selectOption({ label: "Lookup list" });
      await page.locator("#ad-list").selectOption({ label: LIST });
    });

    // Two CIs through the form; the inherited template attributes are there too.
    for (const ci of CIS) {
      await page.goto(`/cis/new?classId=${classId}`);
      await expect(page.locator("#attr-manufacturer")).toBeVisible();
      await page.locator("#attr-name").fill(ci.name);
      await page.locator("#attr-status").selectOption({ label: "In service" });
      await page.locator("#attr-outlets").fill(ci.outlets);
      await page.locator("#attr-tier").selectOption({ label: ci.tier });
      await page.getByRole("button", { name: `Create ${CLASS}` }).click();
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(ci.name);
    }

    // A list view for the new class shows its attributes as columns.
    const s = await apiGet<{ version: number; settings: Record<string, unknown> }>(request, "/ui-settings");
    await apiSend(request, "PUT", "/ui-settings", {
      version: s.version,
      comment: "fresh install",
      settings: {
        branding: { appName: APP, primaryColor: "#0b7a75", accentColor: null, defaultTheme: "light" },
        navigation: { entries: [{ type: "section", key: "power", label: "Power", items: [{ classKey: CLASS_KEY }] }] },
        dashboard: {
          widgets: [
            { id: "power", type: "count_by_class", title: "Power devices", classKeys: [CLASS_KEY] },
            { id: "recent", type: "recent_changes", limit: 5 },
          ],
        },
        listViews: [
          { classKey: CLASS_KEY, columns: ["attributes.name", "attributes.status", "attributes.tier", "attributes.outlets"], defaultSort: { field: "label", direction: "desc" } },
        ],
        layouts: [{ classKey: CLASS_KEY, panels: [{ key: "feed", label: "Power feed", fields: ["attributes.outlets", "attributes.tier"] }], hiddenFields: ["attributes.serial_number"] }],
      },
    });
    await page.goto("/");
    const nav = page.getByRole("navigation", { name: "Main" });
    await expect(nav.getByRole("heading", { name: "Power" })).toBeVisible();
    await nav.getByRole("link", { name: new RegExp(`^${CLASS}`) }).click();
    await expect(page).toHaveURL(new RegExp(`classId=${classId}`));
    // The view leaves out Label, so the inventory shows it first: it is the link that opens the CI.
    await expect(page.locator("table.data thead th")).toHaveText([/Label/, /Name/, /Status/, /Tier/, /Outlets/]);
    const rows = page.locator("table.data tbody tr");
    // Default sort: label (the name), descending.
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0).getByRole("cell").nth(0).getByRole("link")).toHaveText(CIS[1].name);
    await expect(rows.nth(0).getByRole("cell").nth(3)).toHaveText(CIS[1].tier);
    await expect(rows.nth(1).getByRole("cell").nth(4)).toHaveText(CIS[0].outlets);
    await snap(page, "52-own-class-inventory");

    // The inventory filter and the API agree.
    await page.goto(`/cis?q=${CIS[0].name}`);
    await expect(page.locator("table.data tbody tr")).toHaveCount(1);
    const listed = await apiGet<Page_<{ label: string; attributes: Record<string, unknown> }>>(request, `/configuration-items?classId=${classId}&sort=label`);
    expect(listed.data.map((c) => [c.label, c.attributes.outlets])).toEqual(CIS.map((c) => [c.name, Number(c.outlets)]));

    // A permission profile that refers to the class, for the export.
    await apiSend(request, "POST", "/admin/profiles", {
      name: PROFILE,
      description: "Runs the rack PDUs",
      globalPermissions: ["audit.view"],
      classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
    });
  });

  test("the export holds the whole setup and no CIs or users", async ({ request }) => {
    exported = await exportConfig(request);
    expect(exported.format).toBe("shadoucmdb.config");
    expect(exported.dataModel.classes.map((c) => c.key)).toContain(CLASS_KEY);
    expect(exported.dataModel.attributes.filter((a) => a.class === CLASS_KEY).map((a) => a.key)).toEqual(["outlets", "tier"]);
    expect(exported.lookups.lists.find((l) => l.key === "support_tier")?.values.map((v) => v.key)).toEqual(["gold", "silver"]);
    expect(exported.permissionProfiles.map((p) => p.name)).toEqual([PROFILE]);
    expect(exported.uiSettings.settings.branding.appName).toBe(APP);
    const text = JSON.stringify(exported);
    for (const secret of [FRESH_ADMIN.username, FRESH_ADMIN.password, CIS[0].name]) expect(text).not.toContain(secret);
  });
});

test.describe("imported into a fresh install", () => {
  test.use({ baseURL: targetURL, storageState: IMPORT_TARGET_STATE });

  test("the dry run, then apply, rebuild the same setup; exporting it again gives the same file", async ({ page, request }) => {
    // Only the built-in business service type.
    expect((await apiGet<Page_<unknown>>(request, "/ci-classes")).page.total).toBe(1);
    const file = { name: "fresh-install.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(exported)) };

    await page.goto("/admin");
    await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Export / import" }).click();
    await page.locator("#config-file").setInputFiles(file);
    const summary = page.getByRole("table", { name: "Import summary" });
    await expect(summary).toBeVisible();
    // 9 classes in the file; the business service type is already there, built in.
    await expect(summary.getByRole("row", { name: /^CI classes/ }).getByRole("cell").nth(1)).toHaveText("8");
    await expect(summary.getByRole("row", { name: /^Permission profiles/ }).getByRole("cell").nth(1)).toHaveText("1");
    await expect(page.locator(".import-changes").filter({ hasText: "CI classes" })).toContainText(CLASS_KEY);
    // A dry run writes nothing.
    expect((await apiGet<Page_<unknown>>(request, "/ci-classes")).page.total).toBe(1);
    await snap(page, "53-import-into-fresh-dry-run");

    await page.getByRole("button", { name: "Apply import" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Imported fresh-install.json" })).toBeVisible();
    // Branding applies at once.
    await expect(page.locator(".shell-brand")).toContainText(APP);

    // The round trip: the fresh install now exports the same configuration.
    expect(comparable(await exportConfig(request))).toEqual(comparable(exported));
    // Only configuration moved: no CIs, and the source's users stayed behind.
    expect((await apiGet<Page_<unknown>>(request, "/configuration-items")).page.total).toBe(0);
    const users = await apiGet<{ data: { username: string }[] }>(request, "/admin/users");
    expect(users.data.map((u) => u.username)).toEqual([FRESH_ADMIN.username]);

    // Importing the same file again changes nothing.
    await page.locator("#config-file").setInputFiles(file);
    await expect(page.getByText("Importing this file changes nothing")).toBeVisible();
    await expect(page.getByRole("button", { name: "Apply import" })).toBeDisabled();
  });

  test("the imported setup works: template, menu, list view, form and profile", async ({ page, request }) => {
    await page.goto("/admin/templates");
    await expect(page.getByRole("region", { name: "IT infrastructure" }).locator(".badge", { hasText: "Installed" })).toBeVisible();

    await page.goto("/");
    await expect(page).toHaveTitle(`Dashboard · ${APP}`);
    const nav = page.getByRole("navigation", { name: "Main" });
    await expect(nav.getByRole("heading", { name: "Power" })).toBeVisible();
    await nav.getByRole("link", { name: new RegExp(`^${CLASS}`) }).click();
    await expect(page).toHaveURL(/classId=/);
    const classId = new URL(page.url()).searchParams.get("classId")!;

    // A CI of the imported class, with a value from the imported lookup list.
    // The imported form layout: its panel, and the hidden serial number.
    await page.goto(`/cis/new?classId=${classId}`);
    await expect(page.locator("#attr-tier option")).toHaveText(["— not set —", "Gold", "Silver"]);
    await expect(page.getByLabel("Serial number")).toHaveCount(0);
    await page.locator("#attr-name").fill("pdu-ber1-b01");
    await page.locator("#attr-status").selectOption({ label: "In service" });
    await page.locator("#attr-outlets").fill("8");
    await page.locator("#attr-tier").selectOption({ label: "Gold" });
    await page.getByRole("button", { name: `Create ${CLASS}` }).click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("pdu-ber1-b01");
    await expect(page.locator(".layout-panels > details > summary h2").first()).toHaveText(/Power feed/);

    await page.goto(`/cis?classId=${classId}`);
    await expect(page.locator("table.data thead th")).toHaveText([/Label/, /Name/, /Status/, /Tier/, /Outlets/]);
    await expect(page.locator("table.data tbody tr").first().getByRole("cell").nth(3)).toHaveText("Gold");
    await snap(page, "54-imported-class-inventory");

    // The imported dashboard (an empty inventory shows a welcome instead, so only now).
    await page.goto("/");
    await expect(page.locator('[data-widget="power"]').getByRole("heading", { name: /Power devices/ })).toBeVisible();
    await expect(page.locator('[data-widget="recent"]')).toContainText("pdu-ber1-b01");
    await expect(page.locator('[data-widget="by_status"]')).toHaveCount(0);

    // The imported profile points at the imported class, not at the source's id.
    const profiles = await apiGet<{ data: { name: string; classPermissions: { classId: string | null; view: boolean; delete: boolean }[] }[] }>(request, "/admin/profiles");
    const profile = profiles.data.find((p) => p.name === PROFILE);
    expect(profile?.classPermissions).toEqual([expect.objectContaining({ classId, view: true, delete: false })]);
  });
});
