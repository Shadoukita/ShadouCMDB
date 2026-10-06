import AxeBuilder from "@axe-core/playwright";
import { test as base, expect, type APIRequestContext, type Locator, type Page, type TestInfo } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { E2E_USER, FRESH_ADMIN } from "./global-setup";

export { expect };

/**
 * Every test fails on uncaught page errors, console errors and Vue warnings.
 * "Failed to load resource" is excluded: the tests provoke 4xx responses on purpose.
 */
export const test = base.extend<{ failOnPageErrors: void; recentPassword: void }>({
  failOnPageErrors: [
    async ({ page }, use) => {
      const problems: string[] = [];
      page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
      page.on("console", (m) => {
        const text = m.text();
        if (text.startsWith("Failed to load resource")) return;
        if (m.type() === "error" || text.includes("[Vue warn]")) problems.push(`${m.type()}: ${text}`);
      });
      await use();
      expect(problems, "page errors / Vue warnings").toEqual([]);
    },
    { auto: true },
  ],
  /**
   * The suite signs in once and runs far longer than 10 minutes, but changes to users, profiles, API tokens
   * and identity providers need the password confirmed in the last 10 minutes (GH#498): confirm it before
   * each test for the signed-in e2e accounts. A test without a session, or signed in as another account, is left alone.
   */
  recentPassword: [
    async ({ request }, use) => {
      const me = await request.get("/api/v1/auth/me");
      const username = me.ok() ? ((await me.json()) as { user?: { username?: string } }).user?.username : undefined;
      const password = [E2E_USER, FRESH_ADMIN].find((u) => u.username === username)?.password;
      if (password) {
        const csrf = (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
        const res = await request.post("/api/v1/auth/reauthenticate", { data: { currentPassword: password }, headers: { "X-CSRF-Token": csrf } });
        expect(res.status(), `confirming the password of ${username}: ${await res.text()}`).toBe(204);
      }
      await use();
    },
    { auto: true },
  ],
});

/** Saves a full-page screenshot when E2E_SCREENSHOT_DIR is set (evidence for reviews). */
export async function snap(page: Page, name: string) {
  const dir = process.env.E2E_SCREENSHOT_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  await page.screenshot({ path: join(dir, `${name}.png`), fullPage: true });
}

/** Opens the header's user menu (My account, theme, density, sign-out) unless it is already open. Any locale. */
export async function openUserMenu(page: Page) {
  const who = page.locator(".user-menu button.who");
  if ((await who.getAttribute("aria-expanded")) !== "true") await who.click();
}

/** Signs out from the user menu. */
export async function signOut(page: Page) {
  await openUserMenu(page);
  await page.getByRole("button", { name: "Sign out" }).click();
}

/** Picks this browser's theme in the user menu ("" is the administrator's default). */
export async function chooseTheme(page: Page, theme: "" | "light" | "dark" | "system") {
  await openUserMenu(page);
  await page.getByLabel("Theme").selectOption(theme);
}

/**
 * A modal dialog is centred in the viewport, reads left to right and wraps its text inside the box:
 * no line is cut off. A dialog opened from a table row used to inherit the cell's nowrap/ellipsis
 * and sit at the left edge (GH#279).
 */
export async function expectDialogLaidOut(dialog: Locator) {
  await expect(dialog).toBeVisible();
  const box = (await dialog.boundingBox())!;
  const viewport = dialog.page().viewportSize()!;
  expect(Math.abs(box.x + box.width / 2 - viewport.width / 2), "dialog horizontally centred").toBeLessThanOrEqual(2);
  const layout = await dialog.evaluate((d) => {
    const s = getComputedStyle(d);
    const clipped = Array.from(d.querySelectorAll<HTMLElement>(".body p, .body li"))
      .filter((el) => el.scrollWidth > el.clientWidth)
      .map((el) => el.textContent);
    return { textAlign: s.textAlign, whiteSpace: s.whiteSpace, clipped };
  });
  expect(layout).toEqual({ textAlign: expect.stringMatching(/^(start|left)$/), whiteSpace: "normal", clipped: [] });
}

/**
 * A toHaveURL matcher for the exact path and query. A regex like /\/cis\?q=crm$/ also matches
 * "/login?redirect=/cis?q=crm", so it passes before sign-in has finished.
 */
export const at = (pathname: string, search = "") => (url: URL) => url.pathname === pathname && url.search === search;

/** Direct API access through the UI origin (same /api/v1 the app uses). */
export async function apiGet<T>(request: APIRequestContext, path: string): Promise<T> {
  const res = await request.get(`/api/v1${path}`);
  expect(res.ok(), `GET ${path} → ${res.status()}`).toBeTruthy();
  return (await res.json()) as T;
}

export async function apiSend<T>(request: APIRequestContext, method: "POST" | "PUT" | "PATCH", path: string, data: unknown): Promise<T> {
  const csrf = (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
  const res = await request.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": csrf } });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as T;
}

interface Page_<T> {
  data: T[];
}

export async function classIdByName(request: APIRequestContext, name: string): Promise<string> {
  const classes = await apiGet<Page_<{ id: string; name: string }>>(request, "/ci-classes?limit=200");
  const cls = classes.data.find((c) => c.name === name);
  expect(cls, `class ${name}`).toBeTruthy();
  return cls!.id;
}

/** A CI by its label (the value of its class's title attribute: its name, for the template's classes). */
export async function ciIdByName(request: APIRequestContext, name: string): Promise<string> {
  const list = await apiGet<Page_<{ id: string; label: string }>>(request, `/configuration-items?q=${encodeURIComponent(name)}&active=all&limit=50`);
  const ci = list.data.find((c) => c.label === name);
  expect(ci, `CI ${name}`).toBeTruthy();
  return ci!.id;
}

/** The id of a lookup list value by list key and value key, e.g. ("status", "in_service"). */
export async function lookupValueId(request: APIRequestContext, listKey: string, valueKey: string): Promise<string> {
  const lists = await apiGet<Page_<{ id: string; key: string }>>(request, "/lookup-lists?limit=200");
  const list = lists.data.find((l) => l.key === listKey);
  expect(list, `lookup list ${listKey}`).toBeTruthy();
  const values = await apiGet<Page_<{ id: string; key: string }>>(request, `/lookup-list-values?listId=${list!.id}&limit=200`);
  const value = values.data.find((v) => v.key === valueKey);
  expect(value, `lookup value ${listKey}.${valueKey}`).toBeTruthy();
  return value!.id;
}

/** Creates a CI through the API with a name and the status "In service" (the template's required attributes), plus `attributes`. */
export async function createCi(
  request: APIRequestContext,
  classId: string,
  name: string,
  attributes: Record<string, unknown> = {},
): Promise<{ id: string; ident: string; label: string; version: number }> {
  const status = await lookupValueId(request, "status", "in_service");
  return apiSend(request, "POST", "/configuration-items", { classId, attributes: { name, status, ...attributes } });
}

/** Picks a CI in a CiPicker combobox by typing and clicking the option whose name matches exactly. */
export async function pickCi(page: Page, inputSelector: string, search: string, name: string) {
  const input = page.locator(inputSelector);
  await input.fill(search);
  // The picker's own list (aria-controls): a CI page's dropdowns hold options of their own.
  const list = page.locator(`#${await input.getAttribute("aria-controls")}`);
  const option = list.getByRole("option").filter({ has: page.getByText(name, { exact: true }) });
  await option.first().click();
}

// A CI page's fields (SHAA-1644): inputs where the user may change them, values shown read-only otherwise.
/** The labels of the fields in `scope`, in order (a required field's label ends in its asterisk). */
export const fieldLabels = (scope: Locator) => scope.locator(".field > label, .field > .label");
/** The value of a read-only field (`attributes.<key>` or a core field such as `active`). */
export const roValue = (scope: Page | Locator, field: string) => scope.locator(`.field-ro[data-field='${field}'] .ro-value`);
/**
 * What a CI page shows for the field labelled `label`, normalized: a read-only value's text, an input's value,
 * a select's chosen option or a reference picker's chosen CI ("" when not set).
 */
export async function shownValue(page: Page, label: string): Promise<string> {
  const name = new RegExp(`^\\s*${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\*?\\s*$`);
  const field = page.locator(".layout-container .field").filter({ has: page.locator("label, .label").filter({ hasText: name }) });
  const text = await field.first().evaluate((el) => {
    const ro = el.querySelector(".ro-value");
    if (ro) return ro.textContent ?? "";
    const select = el.querySelector("select");
    if (select) return select.value ? (select.selectedOptions[0]?.text ?? "") : "";
    const picked = el.querySelector(".picker-value .picker-name");
    if (picked) return picked.textContent ?? "";
    return el.querySelector<HTMLInputElement | HTMLTextAreaElement>("input, textarea")?.value ?? "";
  });
  return text.replace(/\s+/g, " ").trim();
}
/** Saves the changes made on a CI page with the Save button of its unsaved-changes bar. */
export async function saveCi(page: Page) {
  await page.getByRole("region", { name: "Unsaved changes" }).getByRole("button", { name: "Save", exact: true }).click();
}

/**
 * Confirms the open data model change preview (the dialog showing the DDL a change runs) with its
 * apply button, optionally checking the SQL it shows first.
 */
export async function applySchemaChange(page: Page, button: string, expectSql?: string) {
  const dialog = page.locator("dialog.schema-change[open]");
  await expect(dialog.getByRole("heading", { name: "What it does" })).toBeVisible();
  if (expectSql) await expect(dialog.locator(".sc-ddl")).toContainText(expectSql);
  await dialog.getByRole("button", { name: button, exact: true }).click();
  await expect(dialog).toHaveCount(0);
}

/** The CSRF token of the signed-in API request context, for writes sent with `request` directly. */
export async function csrf(request: APIRequestContext) {
  return (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
}

const BUILT_IN_SETTINGS = {
  branding: { appName: null, primaryColor: null, accentColor: null, defaultTheme: "system" },
  navigation: { entries: [] },
  dashboard: { widgets: null },
  listViews: [],
  layouts: [],
};

/**
 * Saves the layout editor's draft to the template it edits: Save to template opens a confirmation that
 * says who the change reaches, with an optional note for the settings version.
 */
export async function saveLayout(page: Page, note?: string) {
  const bar = page.getByRole("region", { name: "Layout editing" });
  await bar.getByTestId("le-save").click();
  const dialog = page.getByRole("dialog", { name: /^Save to the template/ });
  if (note) await dialog.getByLabel("Note for this version").fill(note);
  await dialog.getByRole("button", { name: "Save to template", exact: true }).click();
  await expect(bar.getByRole("status")).toContainText(/settings version \d+/);
}

/** Back to the built-in UI settings (Customization) and no images, so other specs see the stock UI. */
export async function resetUiSettings(request: APIRequestContext) {
  const s = await apiGet<{ version: number; settings: unknown; assets: { logo: unknown; favicon: unknown } }>(request, "/ui-settings");
  const headers = { "X-CSRF-Token": await csrf(request) };
  if (JSON.stringify(s.settings) !== JSON.stringify(BUILT_IN_SETTINGS)) {
    // Standard sent empty: left out, it keeps the layout it has (GH#521).
    const settings = { layoutTemplates: [{ key: "standard", name: "Standard", layout: {} }] };
    const res = await request.put("/api/v1/ui-settings", { data: { version: s.version, settings, comment: "e2e reset" }, headers });
    expect(res.ok(), `reset → ${res.status()} ${await res.text()}`).toBeTruthy();
  }
  for (const kind of ["logo", "favicon"] as const) {
    if (s.assets[kind]) expect((await request.delete(`/api/v1/ui-settings/assets/${kind}`, { headers })).ok()).toBeTruthy();
  }
}

// ---------- Accessibility ----------

const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const FAILING = new Set(["critical", "serious"]);

/**
 * Runs axe on the page as it is now; fails on critical and serious violations, or on any violation with
 * `strict` (for a screen held to zero, usually narrowed to its region with `include`).
 */
export async function checkA11y(
  page: Page,
  testInfo: TestInfo,
  name: string,
  options: { include?: string; disableRules?: string[]; strict?: boolean } = {},
) {
  let builder = new AxeBuilder({ page }).withTags(WCAG_TAGS);
  if (options.include) builder = builder.include(options.include);
  if (options.disableRules?.length) builder = builder.disableRules(options.disableRules);
  const results = await builder.analyze();
  // "incomplete" are the checks axe could not decide (e.g. contrast over overlapping elements): for a manual look.
  const report = { url: page.url(), violations: results.violations, incomplete: results.incomplete };
  await testInfo.attach(`axe-${name}.json`, { body: JSON.stringify(report, null, 2), contentType: "application/json" });

  const describe = (v: (typeof results.violations)[number]) =>
    `[${v.impact}] ${v.id}: ${v.help} (${v.helpUrl})\n` + v.nodes.map((n) => `    ${n.target.join(" ")}: ${n.failureSummary?.replace(/\s+/g, " ")}`).join("\n");
  const reported = results.violations.filter((v) => !FAILING.has(v.impact ?? ""));
  if (reported.length) {
    testInfo.annotations.push({ type: "a11y (moderate/minor)", description: `${name}: ${reported.map((v) => v.id).join(", ")}` });
    console.log(`axe ${name}: ${reported.length} moderate/minor issue(s), not failing:\n${reported.map(describe).join("\n")}`);
  }
  const failing = results.violations.filter((v) => options.strict || FAILING.has(v.impact ?? ""));
  expect(failing.map(describe), `critical/serious WCAG 2.1 AA violations on ${name}`).toEqual([]);
}
