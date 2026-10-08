import type { APIRequestContext, Browser, Page, Route } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { apiGet, apiSend, at, checkA11y, expect, snap, test, chooseTheme, withInventoryFilters } from "./support";

// Impact analysis (v0.3.0, the Impact tab of a CI): the e2e plan §6.3 and the axe checks §6.4 of the SHAA-883 spec.
// The data is the spec's own, under a stamp: a root "db" that three apps depend on (one of them twice), a fourth app
// reached only through a CI of a class the restricted user may not view, and a CI with no relationships.
//
//   app-a ─needs→ db        app-b ─needs→ app-a      app-c ─needs→ db, app-a
//   secret ─needs→ db       app-d ─needs→ secret     lonely (no relationships)
//
// The relationship type starts as "does not propagate impact"; the first test turns it on in the UI.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "impact-password-123";
const RESTRICTED = `e2e-impact-viewer-${stamp}`;
const TYPE = `Imp needs ${stamp}`;
const N = (s: string) => `imp-${s}-${stamp}`;

const ids: Record<string, string> = {};
let typeId = "";

async function signInUi(browser: Browser, username: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  return page;
}

/** The affected CIs' names in the list view, in order. */
const listed = (page: Page) => page.locator(".impact-table tbody tr:not(.group-row):not(.path-row) td:first-child a").allInnerTexts();
const summary = (page: Page) => page.locator(".impact-summary");
/** The list's group headers ("Imp app … (4)"), without their expand arrow. */
const groupHeaders = (page: Page) =>
  expect.poll(() => page.locator(".impact-table .group-toggle").evaluateAll((els) => els.map((e) => e.textContent!.replace(/[▾▸]/g, "").replace(/\s+/g, " ").trim())));
/** Only the analysis itself (GET …/impact), not its CSV export. */
const isAnalysis = (url: URL) => /^\/api\/v1\/configuration-items\/[^/]+\/impact$/.test(url.pathname);

test.beforeAll(async ({ request }) => {
  const cls = async (name: string, key: string) => {
    const { id } = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key, name });
    const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: id, key: "name", label: "Name", dataType: "text" });
    await apiSend(request, "PATCH", `/ci-classes/${id}`, { titleAttributeId: title.id });
    return id;
  };
  const host = await cls(`Imp host ${stamp}`, `imp_host_${stamp}`);
  const app = await cls(`Imp app ${stamp}`, `imp_app_${stamp}`);
  const secret = await cls(`Imp secret ${stamp}`, `imp_secret_${stamp}`);
  typeId = (
    await apiSend<{ id: string }>(request, "POST", "/relationship-types", { key: `imp_needs_${stamp}`, name: TYPE, forwardLabel: "needs", reverseLabel: "is needed by" })
  ).id;
  for (const [source, target] of [[app, host], [app, app], [secret, host], [app, secret]]) {
    await apiSend(request, "POST", "/relationship-rules", { relationshipTypeId: typeId, sourceClassId: source, targetClassId: target });
  }
  const lists = await apiGet<{ data: { id: string }[] }>(request, "/lookup-lists?systemRole=criticality");
  const values = await apiGet<{ data: { id: string; key: string }[] }>(request, `/lookup-list-values?listId=${lists.data[0].id}&limit=200`);
  const crit = (key: string) => values.data.find((v) => v.key === key)!.id;

  const ci = async (key: string, classId: string, criticalityValueId?: string) =>
    (ids[key] = (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, attributes: { name: N(key) }, criticalityValueId })).id);
  await ci("db", host);
  await ci("lonely", host);
  await ci("app-a", app, crit("critical"));
  await ci("app-b", app, crit("high"));
  await ci("app-c", app);
  await ci("secret", secret);
  await ci("app-d", app);
  const needs = (source: string, target: string) =>
    apiSend(request, "POST", "/relationships", { relationshipTypeId: typeId, sourceCiId: ids[source], targetCiId: ids[target] });
  await needs("app-a", "db");
  await needs("app-b", "app-a");
  await needs("app-c", "db");
  await needs("app-c", "app-a");
  await needs("secret", "db");
  await needs("app-d", "secret");

  // A viewer of the host and app classes only: "secret" and what lies behind it are not theirs to see.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E impact viewers ${stamp}`,
    globalPermissions: [],
    classPermissions: [host, app].map((classId) => ({ classId, view: true, create: false, edit: false, delete: false })),
  });
  await apiSend(request, "POST", "/admin/users", { username: RESTRICTED, email: `${RESTRICTED}@example.test`, displayName: RESTRICTED, password: PASSWORD, profileIds: [profile.id] });
});

test.afterAll(async ({ request }) => {
  // Leave the type propagating nothing, so other specs' analyses are not affected by it.
  if (typeId) await apiSend(request, "PATCH", `/relationship-types/${typeId}`, { impactDirection: "none" });
});

test("1. an administrator sets a relationship type to propagate impact (§6.3.1)", async ({ page }, testInfo) => {
  await page.goto(`/admin/relationships?type=${typeId}`);
  const row = page.getByRole("row").filter({ hasText: TYPE });
  await row.getByRole("button", { name: `Actions for ${TYPE}` }).click();
  await page.getByRole("menuitem", { name: "Edit" }).click();
  const dialog = page.getByRole("dialog");
  const field = dialog.getByLabel("Impact propagation");
  await expect(field).toHaveValue("none");
  // The choices read in the type's own words.
  await expect(field.locator("option")).toHaveText([
    "Does not propagate impact",
    "When the target fails, the source is affected (source needs target)",
    "When the source fails, the target is affected (target is needed by source)",
    "Both ways: either end failing affects the other",
  ]);
  await checkA11y(page, testInfo, "relationship-type-form");
  await snap(page, "relationship-type-impact-field");
  await field.selectOption("target_to_source");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(row.getByRole("cell", { name: "Target → source" })).toBeVisible();
});

test("2. the Impact tab lists the affected CIs, grouped by class, then by criticality (§6.3.2)", async ({ page }, testInfo) => {
  await page.goto(`/cis/${ids.db}`);
  await page.getByRole("tab", { name: "Impact" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`));
  await page.getByLabel("Depth").selectOption("2");
  await expect(summary(page)).toContainText(`5 CIs affected downstream of ${N("db")} within 2 hops · 1 critical · 1 high`);
  // Grouped by class (the default): Imp app, then Imp secret.
  await groupHeaders(page).toEqual([`Imp app ${stamp} (4)`, `Imp secret ${stamp} (1)`]);
  expect(await listed(page)).toEqual([N("app-a"), N("app-c"), N("app-b"), N("app-d"), N("secret")]);
  // "Via" names the last hop, as a link.
  const appB = page.getByRole("row").filter({ has: page.getByRole("link", { name: N("app-b"), exact: true }) });
  await expect(appB).toContainText(`needs ${N("app-a")}`);
  await expect(page.locator("#impact-view-panel")).toHaveAttribute("aria-busy", "false");
  await checkA11y(page, testInfo, "impact-list", { include: ".impact", strict: true });
  await snap(page, "impact-list-by-class");

  await page.getByLabel("Group by").selectOption("criticality");
  await groupHeaders(page).toEqual(["Critical (1)", "High (1)", "Not set (3)"]);
  await snap(page, "impact-list-by-criticality");
  // The criticality badges keep AA contrast in the dark theme too.
  await chooseTheme(page, "dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("#impact-view-panel")).toHaveAttribute("aria-busy", "false");
  await checkA11y(page, testInfo, "impact-list-dark", { include: ".impact", strict: true });
  await snap(page, "impact-list-dark");
  await chooseTheme(page, "");
  // A group collapses and opens from its header.
  const critical = page.getByRole("button", { name: "Critical (1)" });
  await critical.click();
  await expect(critical).toHaveAttribute("aria-expanded", "false");
  expect(await listed(page)).not.toContain(N("app-a"));
  await critical.click();
  expect(await listed(page)).toContain(N("app-a"));
});

test("3. the URL holds the state: reload keeps it, Back restores the previous one (§6.3.3)", async ({ page }) => {
  await page.goto(`/cis/${ids.db}/impact`);
  await page.getByLabel("Depth").selectOption("2");
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`, "?depth=2"));
  await page.getByLabel("Group by").selectOption("hops");
  await page.getByRole("radio", { name: /Upstream/ }).check();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`, "?direction=upstream&depth=2&group=hops"));
  await page.getByRole("radio", { name: /Downstream/ }).check();
  await page.reload();
  await expect(page.getByLabel("Depth")).toHaveValue("2");
  await expect(page.getByLabel("Group by")).toHaveValue("hops");
  await expect(page.getByRole("radio", { name: /Downstream/ })).toBeChecked();
  await groupHeaders(page).toEqual(["1 hop (3)", "2 hops (2)"]);

  await page.goBack();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`, "?direction=upstream&depth=2&group=hops"));
  await expect(page.getByRole("radio", { name: /Upstream/ })).toBeChecked();
  await page.goBack();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`, "?depth=2&group=hops"));
  await expect(page.getByRole("radio", { name: /Downstream/ })).toBeChecked();

  // A hand-edited link: what cannot be used falls back to its default, and the tab says so.
  await page.goto(`/cis/${ids.db}/impact?direction=sideways&depth=99`);
  await expect(page.getByRole("status").filter({ hasText: "could not be used" })).toContainText("direction, depth");
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`));
  await page.goto(`/cis/${ids.db}/impact?inactive=maybe`);
  await expect(page.getByRole("status").filter({ hasText: "could not be used" })).toContainText("The link's inactive CIs could not be used");

  // Unchecking the last chosen type leaves none chosen, not all of them.
  await page.goto(`/cis/${ids.db}/impact?types=${typeId}`);
  await page.locator(".impact-types summary").click();
  await page.getByRole("checkbox", { name: TYPE }).uncheck();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`, "?types=none"));
  await expect(page.locator(".impact-types summary")).toContainText("None");
  await expect(page.getByText("Choose at least one relationship type to follow.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Export CSV" })).toBeDisabled();
  await page.reload();
  await expect(page.getByText("Choose at least one relationship type to follow.")).toBeVisible();
  await page.getByRole("button", { name: "Follow all propagating types" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`));
  await expect(page.locator(".impact-types summary")).toContainText("All propagating types");
});

test("4. the tree view shows the same CIs and walks by keyboard (§6.3.4)", async ({ page }, testInfo) => {
  await page.goto(`/cis/${ids.db}/impact?depth=2&view=tree`);
  const tree = page.getByRole("tree");
  await expect(tree.getByRole("treeitem")).toHaveCount(5);
  // Each CI once, under its shortest-path parent: app-b under app-a, app-d under secret.
  const levels = await tree.getByRole("treeitem").evaluateAll((items) => items.map((i) => `${i.getAttribute("aria-level")} ${i.querySelector("a")!.textContent}`));
  expect(levels).toEqual([`1 ${N("app-a")}`, `2 ${N("app-b")}`, `1 ${N("app-c")}`, `1 ${N("secret")}`, `2 ${N("app-d")}`]);
  await expect(tree.getByRole("treeitem").nth(2)).toContainText("also reached via 1 other relationship");
  await expect(tree.getByRole("treeitem").first().locator(".tree-edge")).toHaveText("is needed by");
  await expect(page.locator("#impact-view-panel")).toHaveAttribute("aria-busy", "false");
  await checkA11y(page, testInfo, "impact-tree", { include: ".impact", strict: true });
  await snap(page, "impact-tree");

  // Keyboard only: into the tree, collapse and expand app-a, down to app-b, open it with Enter.
  await tree.getByRole("treeitem").first().focus();
  await page.keyboard.press("ArrowLeft");
  await expect(tree.getByRole("treeitem").first()).toHaveAttribute("aria-expanded", "false");
  await expect(tree.getByRole("treeitem")).toHaveCount(4);
  await page.keyboard.press("ArrowRight");
  await expect(tree.getByRole("treeitem")).toHaveCount(5);
  await page.keyboard.press("ArrowDown");
  await expect(tree.getByRole("treeitem").nth(1)).toBeFocused();
  await page.keyboard.press("End");
  await expect(tree.getByRole("treeitem").nth(4)).toBeFocused();
  await page.keyboard.press("Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(at(`/cis/${ids["app-b"]}`));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(N("app-b"));
});

test("5. Show path shows the chain as links; a CI in the middle opens (§6.3.5)", async ({ page }) => {
  await page.goto(`/cis/${ids.db}/impact?depth=2&group=none`);
  await page.getByRole("button", { name: `Show the path to ${N("app-b")}` }).click();
  const path = page.getByRole("navigation", { name: `Path to ${N("app-b")}` });
  await expect(path.getByRole("listitem")).toHaveText([N("db"), `is needed by → ${N("app-a")}`, `is needed by → ${N("app-b")}`]);
  await path.getByRole("link", { name: N("app-a") }).click();
  await expect(page).toHaveURL(at(`/cis/${ids["app-a"]}`));
  // The walk shows in the breadcrumb.
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText(N("db"));
});

test("6. a restricted user sees the visibility note, and nothing through a hidden CI (§6.3.6)", async ({ browser }) => {
  const page = await signInUi(browser, RESTRICTED);
  try {
    await page.goto(`/cis/${ids.db}/impact?depth=2`);
    await expect(summary(page)).toContainText(`3 CIs affected downstream of ${N("db")} within 2 hops`);
    await expect(page.getByText("Results include only CIs of classes you are allowed to view.")).toBeVisible();
    expect(await listed(page)).toEqual([N("app-a"), N("app-c"), N("app-b")]);
    await expect(page.locator(".impact")).not.toContainText(N("secret"));
    await expect(page.locator(".impact")).not.toContainText(N("app-d"));
    await snap(page, "impact-restricted");
  } finally {
    await page.context().close();
  }
});

test("7. a truncated result says why, and still exports as CSV (§6.3.7)", async ({ page }, testInfo) => {
  // The server's node limit cannot be reached with a few CIs: the answer is marked truncated on its way in.
  await page.route(isAnalysis, async (route: Route) => {
    const response = await route.fetch();
    const json = { ...(await response.json()), truncated: true, truncatedReason: "max_nodes" };
    await route.fulfill({ response, json });
  });
  await page.goto(`/cis/${ids.db}/impact?depth=2`);
  const banner = page.getByRole("status").filter({ hasText: "Incomplete result." });
  await expect(banner).toContainText("Showing the first 500 affected CIs. Narrow the relationship types or reduce the depth to see a complete result.");
  await expect(page.locator("#impact-view-panel")).toHaveAttribute("aria-busy", "false");
  await checkA11y(page, testInfo, "impact-truncated", { include: ".impact", strict: true });
  await snap(page, "impact-truncated");

  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export CSV" }).click();
  const file = await download;
  const db = await apiGet<{ ident: string }>(page.request, `/configuration-items/${ids.db}`);
  expect(file.suggestedFilename()).toMatch(new RegExp(`^impact-${db.ident}-downstream-\\d{8}-\\d{4}\\.csv$`));
  const lines = (await readFile(await file.path(), "utf8")).split(/\r?\n/);
  expect(lines[0]).toContain(`Impact analysis of ${db.ident} (${N("db")}): direction=downstream, depth=2`);
  expect(lines[1]).toBe('"ci_id","ident","name","class","criticality","direction","hops","via_relationship","via_ci_ident","path_idents","active","status"');
  expect(lines.slice(2).filter(Boolean)).toHaveLength(5);
});

test("8. empty states: nothing affected, and nothing configured, worded for administrators and others (§6.3.8)", async ({ page, browser }, testInfo) => {
  await page.goto(`/cis/${ids.lonely}/impact`);
  await expect(page.getByRole("heading", { name: `No CIs are affected downstream of ${N("lonely")} within 3 hops.` })).toBeVisible();
  await expect(page.getByText("Try Upstream or increase the depth.")).toBeVisible();
  await checkA11y(page, testInfo, "impact-empty", { include: ".impact", strict: true });

  // Every type set to "none" (a fresh or customised instance), as the settings report it.
  const notConfigured = async (p: Page) =>
    p.route("**/api/v1/settings/impact", async (route: Route) => {
      const response = await route.fetch();
      await route.fulfill({ response, json: { ...(await response.json()), anyTypePropagates: false } });
    });
  await notConfigured(page);
  await page.goto(`/cis/${ids.db}/impact`);
  await expect(page.getByRole("heading", { name: "Impact analysis is not configured" })).toBeVisible();
  await expect(page.locator(".impact .state")).toContainText("No relationship type is set to propagate impact yet.");
  await expect(page.getByRole("link", { name: "Data model › Relationship types" })).toHaveAttribute("href", "/admin/relationships");

  const viewer = await signInUi(browser, RESTRICTED);
  try {
    await notConfigured(viewer);
    await viewer.goto(`/cis/${ids.db}/impact`);
    await expect(viewer.locator(".impact .state")).toContainText("Ask an administrator to configure which relationship types propagate impact.");
    await expect(viewer.getByRole("link", { name: "Data model › Relationship types" })).toHaveCount(0);
  } finally {
    await viewer.context().close();
  }
});

test("9. entry points: the relationship map, the inventory row, the search result and the header (§6.3.9)", async ({ page }) => {
  await page.goto(`/cis/${ids.db}`);
  await page.getByRole("tab", { name: "Relationship map" }).click();
  await page.getByLabel("Direction").selectOption("incoming");
  // Each node's action is named for what it does; the CI it acts on is its description (and its row).
  const action = page.getByRole("treeitem").filter({ has: page.getByRole("link", { name: N("app-a"), exact: true }) }).getByRole("link", { name: "Analyse impact" });
  await expect(action).toHaveAttribute("title", `Analyse the impact of ${N("app-a")}`);
  await action.click();
  await expect(page).toHaveURL(at(`/cis/${ids["app-a"]}/impact`));
  await expect(page.getByRole("tab", { name: "Impact" })).toHaveAttribute("aria-selected", "true");

  await page.goto(`/cis?q=${N("db")}`);
  // The row menu holds Impact analysis (no longer a button on every row).
  await page.getByRole("row").filter({ hasText: N("db") }).getByRole("button", { name: `Actions for ${N("db")}` }).click();
  await page.getByRole("menuitem", { name: "Impact analysis" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids.db}/impact`));

  await page.goto(`/search?q=${N("app-c")}`);
  await page.getByRole("row").filter({ hasText: N("app-c") }).getByRole("button", { name: `Actions for ${N("app-c")}` }).click();
  await page.getByRole("menuitem", { name: "Impact analysis" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids["app-c"]}/impact`));

  await page.goto(`/cis/${ids.lonely}`);
  await page.getByRole("link", { name: "Impact analysis", exact: true }).click();
  await expect(page).toHaveURL(at(`/cis/${ids.lonely}/impact`));
  // Another tab leaves the Impact URL.
  await page.getByRole("tab", { name: "Relationship map" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids.lonely}`));
});

test("10. a busy server says so, with Retry (§6.3.10)", async ({ page }) => {
  let refuse = true;
  await page.route(isAnalysis, async (route: Route) => {
    if (!refuse) return route.continue();
    await route.fulfill({
      status: 429,
      json: { error: { code: "RATE_LIMITED", message: "Too many impact analyses at once", details: [], requestId: "e2e-busy" } },
    });
  });
  await page.goto(`/cis/${ids.db}/impact`);
  const alert = page.getByRole("alert").filter({ hasText: "The server is busy" });
  await expect(alert).toContainText("Try again in a moment.");
  refuse = false;
  await alert.getByRole("button", { name: "Retry" }).click();
  await expect(summary(page)).toContainText(`affected downstream of ${N("db")}`);
});

test("criticality: set on the CI form, shown as a column and filtered in the inventory", async ({ page, request }: { page: Page; request: APIRequestContext }) => {
  await page.goto(`/cis/${ids["app-c"]}/edit`);
  await page.getByLabel("Criticality").selectOption({ label: "Medium" });
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(at(`/cis/${ids["app-c"]}`));
  await expect(page.locator(".page-header .badge.criticality")).toHaveText("Medium");
  const saved = await apiGet<{ criticality: { key: string } | null }>(request, `/configuration-items/${ids["app-c"]}`);
  expect(saved.criticality?.key).toBe("medium");

  await page.goto(`/cis?q=${stamp}&columns=label,criticality`);
  await withInventoryFilters(page, () => page.getByLabel("Criticality", { exact: true }).selectOption({ label: "Critical" }));
  await expect(page).toHaveURL(/criticalityValueId=/);
  await expect(page.locator("table.data tbody tr")).toHaveCount(1);
  await expect(page.locator("table.data tbody tr").first()).toContainText(N("app-a"));
  await expect(page.locator("table.data tbody tr .crit-meter")).toHaveText("Critical");
});
