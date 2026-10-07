import type { APIRequestContext, APIResponse, Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { AREAS_IMPORT_STATE, AREAS_STATE } from "./global-setup";
import { apiGet, apiSend, applySchemaChange, csrf, expect, snap, test } from "./support";

// An area is a PostgreSQL schema and each of its types a real table: "Bestand" with "Netzwerk" and "Virtuelle
// Maschinen" becomes bestand.netzwerk and bestand.virtuelle_maschinen, one typed column per field. Everything is
// done in the UI and then checked in PostgreSQL itself (psql with the PG* environment, as the app's database
// owner), not only through the API. Two more APIs on their own migrated and `seed`ed databases (no user; global
// setup creates FRESH_ADMIN on both): E2E_AREAS_BASE_URL is built up, E2E_AREAS_IMPORT_BASE_URL receives its export.
// E2E_AREAS_PGDATABASE / E2E_AREAS_IMPORT_PGDATABASE name their databases. A run consumes them; CI creates both.
const sourceURL = process.env.E2E_AREAS_BASE_URL;
const targetURL = process.env.E2E_AREAS_IMPORT_BASE_URL;
const sourceDb = process.env.E2E_AREAS_PGDATABASE;
const targetDb = process.env.E2E_AREAS_IMPORT_PGDATABASE;
test.skip(
  !sourceURL || !targetURL || !sourceDb || !targetDb,
  "E2E_AREAS_BASE_URL, E2E_AREAS_IMPORT_BASE_URL and their E2E_AREAS_PGDATABASE / E2E_AREAS_IMPORT_PGDATABASE are not set",
);
test.describe.configure({ mode: "serial" });

const RESTRICTED = { username: "bestand-viewer", password: "bestand-viewer-password-1" };
// The types have no name field and no title attribute, so their assets are labelled by their generated ident.
// In VLAN order.
const NETWORKS = [
  { vlan: "110", subnet: "10.10.0.0/24", gateway: "10.10.0.1" },
  { vlan: "120", subnet: "10.20.0.0/24", gateway: "10.20.0.1" },
];
// In vCPU order. The second is left without a commissioning date and with a hypervisor name that is no number.
const VMS = [
  { vcpu: "4", ram: "16", hypervisor: "4711", commissioned: "2026-03-01", monitored: true },
  { vcpu: "8", ram: "32.5", hypervisor: "esx-fra1-07", commissioned: "", monitored: false },
];

let areaId = "";
let networkClassId = "";
let vmClassId = "";
/** The source's export, taken once the model is built. */
let exported: { dataModel: { areas: { key: string }[]; classes: { key: string; area?: string }[] } };

interface Column {
  column: string;
  type: string;
  nullable: boolean;
}

/** Runs one query in `db` and returns its rows, read as JSON (the query is built from the test's own constants only). */
function sql<T>(db: string, query: string): T[] {
  const out = execFileSync("psql", ["-XAtq", "-v", "ON_ERROR_STOP=1", "-d", db, "-c", `SELECT coalesce(json_agg(q), '[]') FROM (${query}) q`], {
    encoding: "utf8",
  });
  return JSON.parse(out.trim()) as T[];
}

/** The columns of a table or view, in order, as PostgreSQL reports them. */
function columns(db: string, schema: string, table: string): Column[] {
  return sql<Column>(
    db,
    `SELECT column_name AS column, data_type AS type, is_nullable = 'YES' AS nullable FROM information_schema.columns
     WHERE table_schema = '${schema}' AND table_name = '${table}' ORDER BY ordinal_position`,
  );
}

const relkind = (db: string, qualified: string) =>
  sql<{ kind: string }>(db, `SELECT relkind::text AS kind FROM pg_class WHERE oid = to_regclass('${qualified}')`)[0]?.kind ?? null;

/** What a type table must look like: the registry id, then one typed column per field. */
const NETWORK_COLUMNS: Column[] = [
  { column: "id", type: "uuid", nullable: false },
  { column: "vlan_id", type: "text", nullable: true },
  { column: "subnetz", type: "cidr", nullable: true },
  { column: "gateway", type: "inet", nullable: true },
];
const VM_COLUMNS: Column[] = [
  { column: "id", type: "uuid", nullable: false },
  { column: "vcpu", type: "bigint", nullable: true },
  { column: "arbeitsspeicher_gb", type: "numeric", nullable: true },
  { column: "hypervisor", type: "text", nullable: true },
  { column: "inbetriebnahme", type: "date", nullable: true },
  { column: "ueberwacht", type: "boolean", nullable: true },
];

async function addField(page: Page, label: string, key: string, type: string) {
  await page.getByRole("button", { name: "Add attribute", exact: true }).click();
  await page.locator("#ad-label").fill(label);
  // The technical name is derived from the label (umlauts transliterated) and previewed before anything runs.
  await expect(page.locator("#ad-key")).toHaveValue(key);
  await page.locator("#ad-type").selectOption({ label: type });
  await page.getByRole("button", { name: "Preview and add…" }).click();
  await applySchemaChange(page, "Add attribute", `ADD COLUMN "${key}"`);
  await expect(page.getByRole("status").filter({ hasText: `Added attribute ${label}.` })).toBeVisible();
}

async function createType(page: Page, name: string, key: string): Promise<string> {
  await page.goto("/admin/areas");
  await page.getByRole("row", { name: /Bestand/ }).getByRole("link", { name: "Class in Bestand" }).click();
  await page.locator("#class-name").fill(name);
  await expect(page.locator("#class-key")).toHaveValue(key);
  await expect(page.locator("#class-area")).toHaveValue(areaId);
  await expect(page.locator("#class-key-hint")).toContainText(`bestand.${key}`);
  await page.getByRole("button", { name: "Create class" }).click();
  await applySchemaChange(page, "Create class", `CREATE TABLE "bestand"."${key}"`);
  await expect(page.getByRole("status").filter({ hasText: `Created class ${name} (table bestand.${key}).` })).toBeVisible();
  return page.url().split("/").pop()!;
}

/** Opens the edit dialog of one field on its class page. */
async function editField(page: Page, classId: string, label: string) {
  await page.goto(`/admin/classes/${classId}`);
  await page.locator("table.attributes").getByRole("button", { name: label, exact: true }).click();
  await expect(page.getByRole("heading", { name: `Edit attribute “${label}”` })).toBeVisible();
}

async function expectRefused(res: APIResponse, what: string) {
  expect(res.status(), `${what}: ${res.status()} ${await res.text()}`).toBe(403);
  expect((await res.json()).error.code, what).toBe("FORBIDDEN");
}

test.describe("the source install", () => {
  test.use({ baseURL: sourceURL, storageState: AREAS_STATE });

  test("the area Bestand is a menu tab and the PostgreSQL schema bestand", async ({ page, request }) => {
    expect(sql(sourceDb!, "SELECT 1 FROM information_schema.schemata WHERE schema_name = 'bestand'")).toEqual([]);
    await page.goto("/admin/areas");
    await page.getByRole("button", { name: "New area", exact: true }).first().click();
    await page.locator("#area-name").fill("Bestand");
    await expect(page.locator("#area-key")).toHaveValue("bestand");
    await expect(page.locator("#area-key-hint")).toContainText("Will be created as bestand");
    await page.getByRole("button", { name: "Preview and create…" }).click();
    await applySchemaChange(page, "Create area", 'CREATE SCHEMA "bestand"');
    await expect(page.getByRole("status").filter({ hasText: "Created area Bestand (schema bestand)." })).toBeVisible();
    await expect(page.getByRole("row", { name: /Bestand/ }).getByRole("cell", { name: "bestand", exact: true })).toBeVisible();

    const areas = await apiGet<{ data: { id: string; key: string; name: string }[] }>(request, "/areas");
    areaId = areas.data.find((a) => a.key === "bestand")!.id;
    expect(sql(sourceDb!, "SELECT schema_name AS name FROM information_schema.schemata WHERE schema_name = 'bestand'")).toEqual([{ name: "bestand" }]);
  });

  test("Netzwerk and Virtuelle Maschinen are tables in bestand with one typed column per field", async ({ page }) => {
    networkClassId = await createType(page, "Netzwerk", "netzwerk");
    await addField(page, "VLAN-ID", "vlan_id", "Text");
    await addField(page, "Subnetz", "subnetz", "Network (CIDR)");
    await addField(page, "Gateway", "gateway", "IP address");

    vmClassId = await createType(page, "Virtuelle Maschinen", "virtuelle_maschinen");
    await addField(page, "vCPU", "vcpu", "Whole number");
    await addField(page, "Arbeitsspeicher (GB)", "arbeitsspeicher_gb", "Decimal number");
    await addField(page, "Hypervisor", "hypervisor", "Text");
    await addField(page, "Inbetriebnahme", "inbetriebnahme", "Date");
    await addField(page, "Überwacht", "ueberwacht", "Yes / no");
    await expect(page.locator("#class-db-title")).toBeVisible();
    await snap(page, "60-bestand-vm-type");

    // The tables, as PostgreSQL has them.
    expect(columns(sourceDb!, "bestand", "netzwerk")).toEqual(NETWORK_COLUMNS);
    expect(columns(sourceDb!, "bestand", "virtuelle_maschinen")).toEqual(VM_COLUMNS);
    expect(relkind(sourceDb!, "bestand.netzwerk")).toBe("r");
    expect(relkind(sourceDb!, "bestand.virtuelle_maschinen")).toBe("r");
    // Class-table inheritance: the id is the registry's, and deleting the CI deletes the row.
    for (const table of ["netzwerk", "virtuelle_maschinen"]) {
      expect(
        sql(
          sourceDb!,
          `SELECT confrelid::regclass::text AS registry, confdeltype::text AS on_delete FROM pg_constraint
           WHERE conrelid = 'bestand.${table}'::regclass AND contype = 'f' AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = 'bestand.${table}'::regclass AND attname = 'id')]::int2[]`,
        ),
        table,
      ).toEqual([{ registry: "cmdb.configuration_items", on_delete: "c" }]);
    }
    // A read-only reporting view per type: the registry columns, then the type's own.
    expect(relkind(sourceDb!, "bestand.v_netzwerk")).toBe("v");
    expect(relkind(sourceDb!, "bestand.v_virtuelle_maschinen")).toBe("v");
    const view = columns(sourceDb!, "bestand", "v_netzwerk").map((c) => c.column);
    expect(view).toEqual(expect.arrayContaining(["id", "ident", "label", "valid_from", "valid_until", "active", "vlan_id", "subnetz", "gateway"]));
    expect(view.slice(-3)).toEqual(["vlan_id", "subnetz", "gateway"]);

    // Every DDL that ran is in the change history, with its exact statements.
    const history = sql<{ summary: string; statements: string[] }>(
      sourceDb!,
      "SELECT summary, statements FROM cmdb.schema_changes WHERE array_to_string(statements, ' ') LIKE '%\"bestand\"%' ORDER BY occurred_at, id",
    );
    expect(history).toHaveLength(1 + 1 + 3 + 1 + 5);
    expect(history[0].statements.join("\n")).toContain('CREATE SCHEMA "bestand"');
  });

  test("assets created in the UI are rows of the type tables and the reporting views", async ({ page, request }) => {
    for (const n of NETWORKS) {
      await page.goto(`/cis/new?classId=${networkClassId}`);
      await page.locator("#attr-vlan_id").fill(n.vlan);
      await page.locator("#attr-subnetz").fill(n.subnet);
      await page.locator("#attr-gateway").fill(n.gateway);
      await page.getByRole("button", { name: "Create Netzwerk" }).click();
      await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
      await expect(page.getByRole("heading", { level: 1 })).toHaveText(/\S/);
    }
    for (const v of VMS) {
      await page.goto(`/cis/new?classId=${vmClassId}`);
      await page.locator("#attr-vcpu").fill(v.vcpu);
      await page.locator("#attr-arbeitsspeicher_gb").fill(v.ram);
      await page.locator("#attr-hypervisor").fill(v.hypervisor);
      if (v.commissioned) await page.locator("#attr-inbetriebnahme").fill(v.commissioned);
      await page.locator("#attr-ueberwacht").selectOption(v.monitored ? "true" : "false");
      await page.getByRole("button", { name: "Create Virtuelle Maschinen" }).click();
      await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
    }
    await snap(page, "61-bestand-vm-detail");

    // The rows, joined to the registry by id, with typed values.
    expect(
      sql(
        sourceDb!,
        `SELECT ci.label = ci.ident AS labelled_by_ident, n.vlan_id, n.subnetz::text AS subnetz, host(n.gateway) AS gateway
         FROM bestand.netzwerk n JOIN cmdb.configuration_items ci USING (id) ORDER BY n.vlan_id`,
      ),
    ).toEqual(NETWORKS.map((n) => ({ labelled_by_ident: true, vlan_id: n.vlan, subnetz: n.subnet, gateway: n.gateway })));
    expect(
      sql(
        sourceDb!,
        `SELECT vcpu::text AS vcpu, arbeitsspeicher_gb::text AS ram, hypervisor, inbetriebnahme::text AS commissioned, ueberwacht AS monitored, active
         FROM bestand.v_virtuelle_maschinen ORDER BY vcpu`,
      ),
    ).toEqual(VMS.map((v) => ({ vcpu: v.vcpu, ram: v.ram, hypervisor: v.hypervisor, commissioned: v.commissioned || null, monitored: v.monitored, active: true })));
    expect(sql(sourceDb!, "SELECT label = ident AS labelled_by_ident, vlan_id FROM bestand.v_netzwerk ORDER BY vlan_id")).toEqual(
      NETWORKS.map((n) => ({ labelled_by_ident: true, vlan_id: n.vlan })),
    );

    // The API and the inventory show the same.
    const listed = await apiGet<{ data: { ident: string; label: string; attributes: Record<string, unknown> }[] }>(request, `/configuration-items?classId=${vmClassId}&sort=createdAt`);
    expect(listed.data.map((c) => [c.label === c.ident, c.attributes.hypervisor])).toEqual(VMS.map((v) => [true, v.hypervisor]));
    await page.goto("/");
    const nav = page.getByRole("navigation", { name: "Main" });
    await expect(nav.getByRole("heading", { name: "Bestand" })).toBeVisible();
    await nav.getByRole("link", { name: /^Netzwerk/ }).click();
    await expect(page.locator("table.data tbody tr")).toHaveCount(NETWORKS.length);
    await snap(page, "62-bestand-inventory");
  });

  test("a field type change converts the column, and one that would not convert is refused", async ({ page }) => {
    // VLAN-ID: text → whole number. Every stored value converts, so the column becomes bigint and keeps them.
    await editField(page, networkClassId, "VLAN-ID");
    await page.locator("#ad-type").selectOption({ label: "Whole number" });
    await page.getByRole("button", { name: "Save attribute" }).click();
    await applySchemaChange(page, "Save attribute", 'ALTER COLUMN "vlan_id" TYPE bigint');
    await expect(page.getByRole("status").filter({ hasText: "Saved attribute VLAN-ID." })).toBeVisible();
    expect(columns(sourceDb!, "bestand", "netzwerk").find((c) => c.column === "vlan_id")?.type).toBe("bigint");
    expect(sql(sourceDb!, "SELECT vlan_id FROM bestand.netzwerk ORDER BY vlan_id")).toEqual([{ vlan_id: 110 }, { vlan_id: 120 }]);

    // Hypervisor: text → whole number. "esx-fra1-07" is no number, so the dry run refuses and nothing changes.
    await editField(page, vmClassId, "Hypervisor");
    await page.locator("#ad-type").selectOption({ label: "Whole number" });
    await page.getByRole("button", { name: "Save attribute" }).click();
    const dialog = page.locator("dialog.schema-change[open]");
    await expect(dialog.getByRole("alert")).toContainText("Refused: this change would lose or break stored data");
    await expect(dialog.getByRole("alert")).toContainText("esx-fra1-07");
    await expect(dialog.getByRole("button", { name: "Save attribute" })).toHaveCount(0);
    await snap(page, "63-type-change-refused");
    await dialog.getByRole("button", { name: "Close" }).click();
    expect(columns(sourceDb!, "bestand", "virtuelle_maschinen").find((c) => c.column === "hypervisor")?.type).toBe("text");
    expect(sql(sourceDb!, "SELECT hypervisor FROM bestand.virtuelle_maschinen ORDER BY hypervisor")).toEqual([{ hypervisor: "4711" }, { hypervisor: "esx-fra1-07" }]);
  });

  test("a field cannot be made required while an asset has no value", async ({ page }) => {
    await editField(page, vmClassId, "Inbetriebnahme");
    await page.locator("#ad-required").check();
    await page.getByRole("button", { name: "Save attribute" }).click();
    const dialog = page.locator("dialog.schema-change[open]");
    await expect(dialog.getByRole("alert")).toContainText("Refused: this change would lose or break stored data");
    await expect(dialog.getByRole("button", { name: "Save attribute" })).toHaveCount(0);
    await dialog.getByRole("button", { name: "Close" }).click();
    expect(columns(sourceDb!, "bestand", "virtuelle_maschinen").find((c) => c.column === "inbetriebnahme")?.nullable).toBe(true);
  });

  test("an archived field keeps its column; purging it, typed to confirm, drops the column", async ({ page }) => {
    await page.goto(`/admin/classes/${vmClassId}`);
    await page.getByRole("button", { name: "Actions for Überwacht" }).click();
    await page.getByRole("menuitem", { name: "Archive" }).click();
    await applySchemaChange(page, "Archive attribute");
    await expect(page.getByRole("status").filter({ hasText: "Archived Überwacht" })).toBeVisible();
    // Archived: hidden from forms, but the column and its values are still there.
    expect(sql(sourceDb!, "SELECT ueberwacht FROM bestand.virtuelle_maschinen WHERE ueberwacht")).toEqual([{ ueberwacht: true }]);
    await page.goto(`/cis/new?classId=${vmClassId}`);
    await expect(page.locator("#attr-vcpu")).toBeVisible();
    await expect(page.locator("#attr-ueberwacht")).toHaveCount(0);

    await page.goto(`/admin/classes/${vmClassId}`);
    await page.getByRole("button", { name: "Actions for Überwacht" }).click();
    await page.getByRole("menuitem", { name: "Purge…" }).click();
    const dialog = page.locator("dialog.schema-change[open]");
    await expect(dialog.locator(".sc-ddl")).toContainText('DROP COLUMN "ueberwacht"');
    const purge = dialog.getByRole("button", { name: "Purge attribute" });
    await expect(purge).toBeDisabled();
    await dialog.getByLabel(/Type .* to confirm/).fill("Überwacht");
    await expect(purge).toBeDisabled();
    await dialog.getByLabel(/Type .* to confirm/).fill("ueberwacht");
    await snap(page, "64-field-purge");
    await purge.click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status").filter({ hasText: "Purged Überwacht: column ueberwacht was dropped." })).toBeVisible();

    const expected = VM_COLUMNS.filter((c) => c.column !== "ueberwacht");
    expect(columns(sourceDb!, "bestand", "virtuelle_maschinen")).toEqual(expected);
    // The reporting view was rebuilt without it.
    expect(columns(sourceDb!, "bestand", "v_virtuelle_maschinen").map((c) => c.column)).not.toContain("ueberwacht");
  });

  test("a user without the manage-data-model right can change none of it", async ({ page, playwright, request }) => {
    // Every CI permission on both types, but no global permission: no datamodel.manage.
    const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: "Bestand editors",
      globalPermissions: [],
      classPermissions: [networkClassId, vmClassId].map((classId) => ({ classId, view: true, create: true, edit: true, delete: true })),
    });
    await apiSend(request, "POST", "/admin/users", { ...RESTRICTED, email: `${RESTRICTED.username}@example.test`, displayName: "Bestand viewer", profileIds: [profile.id] });
    const before = { net: columns(sourceDb!, "bestand", "netzwerk"), vm: columns(sourceDb!, "bestand", "virtuelle_maschinen") };
    const schemas = () => sql(sourceDb!, "SELECT schema_name FROM information_schema.schemata ORDER BY 1");
    const schemasBefore = schemas();
    const changesBefore = sql(sourceDb!, "SELECT count(*)::int AS n FROM cmdb.schema_changes");

    const ctx: APIRequestContext = await playwright.request.newContext({ baseURL: sourceURL, storageState: { cookies: [], origins: [] } });
    const login = await ctx.post("/api/v1/auth/login", { data: RESTRICTED });
    expect(login.status(), await login.text()).toBe(200);
    const headers = { "X-CSRF-Token": (await login.json()).csrfToken as string };
    const send = (method: string, path: string, data?: unknown) => ctx.fetch(`/api/v1${path}`, { method, data, headers });
    const fields = await apiGet<{ data: { id: string; key: string }[] }>(request, `/attribute-definitions?classId=${vmClassId}`);
    const hypervisor = fields.data.find((f) => f.key === "hypervisor")!.id;

    await expectRefused(await send("POST", "/areas", { name: "Schatten", key: "schatten", sortOrder: 99 }), "create an area");
    await expectRefused(await send("PATCH", `/areas/${areaId}`, { name: "Umbenannt" }), "rename an area");
    await expectRefused(await send("DELETE", `/areas/${areaId}`), "archive an area");
    await expectRefused(await send("POST", `/areas/${areaId}/purge`, { confirm: "bestand" }), "purge an area");
    await expectRefused(await send("POST", "/ci-classes", { name: "Speicher", key: "speicher", areaId, sortOrder: 99 }), "create a type");
    await expectRefused(await send("POST", `/ci-classes/${networkClassId}/purge`, { confirm: "netzwerk" }), "purge a type");
    await expectRefused(await send("POST", "/attribute-definitions", { classId: vmClassId, key: "cluster", label: "Cluster", dataType: "text", sortOrder: 99 }), "add a field");
    await expectRefused(await send("PATCH", `/attribute-definitions/${hypervisor}`, { dataType: "integer" }), "change a field type");
    await expectRefused(await send("PATCH", `/attribute-definitions/${hypervisor}`, { isRequired: true }), "make a field required");
    await expectRefused(await send("DELETE", `/attribute-definitions/${hypervisor}`), "archive a field");
    await expectRefused(await send("POST", `/attribute-definitions/${hypervisor}/purge`, { confirm: "hypervisor" }), "purge a field");
    await expectRefused(await send("POST", "/schema-changes/preview", { operation: "createArea", body: { name: "Schatten", key: "schatten", sortOrder: 99 } }), "preview a change");
    await expectRefused(await send("POST", "/admin/config/import?dryRun=true", { format: "shadoucmdb.config" }), "import a configuration");
    // But the assets themselves are theirs to read.
    expect((await ctx.get(`/api/v1/configuration-items?classId=${networkClassId}`)).status()).toBe(200);
    await ctx.dispose();

    // Nothing changed in the database.
    expect(columns(sourceDb!, "bestand", "netzwerk")).toEqual(before.net);
    expect(columns(sourceDb!, "bestand", "virtuelle_maschinen")).toEqual(before.vm);
    expect(schemas()).toEqual(schemasBefore);
    expect(sql(sourceDb!, "SELECT count(*)::int AS n FROM cmdb.schema_changes")).toEqual(changesBefore);

    // And the UI offers none of it: no Administration, and the data model pages refuse.
    const context = await page.context().browser()!.newContext({ baseURL: sourceURL, storageState: { cookies: [], origins: [] } });
    const ui = await context.newPage();
    await ui.goto("/login");
    await ui.getByLabel("Username").fill(RESTRICTED.username);
    await ui.getByLabel("Password").fill(RESTRICTED.password);
    await ui.getByRole("button", { name: "Sign in" }).click();
    await expect(ui.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
    await expect(ui.getByRole("navigation", { name: "Main" }).getByRole("heading", { name: "Bestand" })).toBeVisible();
    await expect(ui.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Administration", exact: true })).toHaveCount(0);
    for (const path of ["/admin/areas", "/admin/classes/new", `/admin/classes/${vmClassId}`]) {
      await ui.goto(path);
      await expect(ui.getByRole("heading", { name: "You do not have access to Administration" }), path).toBeVisible();
      await expect(ui.getByRole("button", { name: "New area", exact: true }), path).toHaveCount(0);
      await expect(ui.getByRole("button", { name: "Add attribute", exact: true }), path).toHaveCount(0);
    }
    await snap(ui, "65-restricted-no-data-model");
    await context.close();
  });

  test("the export carries the area and the technical names", async ({ request }) => {
    const res = await request.get("/api/v1/admin/config/export", { headers: { "X-CSRF-Token": await csrf(request) } });
    expect(res.ok(), `export → ${res.status()}`).toBeTruthy();
    exported = await res.json();
    expect(exported.dataModel.areas.map((a) => a.key)).toContain("bestand");
    expect(exported.dataModel.classes.map((c) => c.key)).toEqual(expect.arrayContaining(["netzwerk", "virtuelle_maschinen"]));
    // Assets are not configuration.
    expect(JSON.stringify(exported)).not.toContain(VMS[1].hypervisor);
  });
});

test.describe("imported into a fresh install", () => {
  test.use({ baseURL: targetURL, storageState: AREAS_IMPORT_STATE });

  test("the dry run shows the DDL; applying it rebuilds the same schema, tables and views", async ({ page }) => {
    expect(sql(targetDb!, "SELECT 1 FROM information_schema.schemata WHERE schema_name = 'bestand'")).toEqual([]);
    const file = { name: "bestand.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(exported)) };
    await page.goto("/admin/config");
    await page.locator("#config-file").setInputFiles(file);
    await expect(page.getByRole("heading", { name: "Database changes" })).toBeVisible();
    const ddl = page.locator(".import-changes .sc-ddl");
    await expect(ddl.filter({ hasText: 'CREATE SCHEMA "bestand"' })).toHaveCount(1);
    await expect(ddl.filter({ hasText: 'CREATE TABLE "bestand"."netzwerk"' })).toHaveCount(1);
    await expect(ddl.filter({ hasText: 'CREATE TABLE "bestand"."virtuelle_maschinen"' })).toHaveCount(1);
    // A dry run creates nothing.
    expect(sql(targetDb!, "SELECT 1 FROM information_schema.schemata WHERE schema_name = 'bestand'")).toEqual([]);
    await snap(page, "66-import-dry-run-ddl");

    await page.getByRole("button", { name: "Apply import" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Imported bestand.json" })).toBeVisible();

    // The same tables, column for column (the converted type and the purge included), and the same views.
    for (const table of ["netzwerk", "virtuelle_maschinen", "v_netzwerk", "v_virtuelle_maschinen"]) {
      expect(columns(targetDb!, "bestand", table), table).toEqual(columns(sourceDb!, "bestand", table));
    }
    expect(columns(targetDb!, "bestand", "netzwerk").find((c) => c.column === "vlan_id")?.type).toBe("bigint");
    expect(columns(targetDb!, "bestand", "virtuelle_maschinen").map((c) => c.column)).not.toContain("ueberwacht");
    expect(relkind(targetDb!, "bestand.v_netzwerk")).toBe("v");
    // No assets came along.
    expect(sql(targetDb!, "SELECT count(*)::int AS n FROM bestand.netzwerk")).toEqual([{ n: 0 }]);

    // And the imported type takes assets.
    await page.goto("/");
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Netzwerk/ }).click();
    await expect(page).toHaveURL(/classId=/);
  });
});
