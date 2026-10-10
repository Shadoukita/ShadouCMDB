import type { Locator, Page } from "@playwright/test";
import { de } from "../src/i18n/de";
import { en, type MessageKey } from "../src/i18n/en";
import { checkA11y, expect, openUserMenu, resetUiSettings, test } from "./support";

// The app shell, the dashboard and the shared components take every text from the message catalog
// (SHAA-1460). With the test-only German locale forced, none of their English texts may show: a
// leftover hard-coded string or a missing `t()` would. The UI itself stays English; there is no setting.

const SLICE = /^(shell|nav|userMenu|globalSearch|notFound|pagination|dashboard|error|formError|time|dataModel)\./;

/** This slice's English texts that read differently in German (messages with parameters are checked by the assertions). */
const ENGLISH = (Object.keys(en) as MessageKey[])
  .filter((k) => SLICE.test(k) && en[k] !== de[k] && !en[k].includes("{") && en[k].length >= 4)
  .map((k) => en[k]);

async function forceGerman(page: Page) {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
}

/** Whole words only, so a CI or class name that merely contains an English word does not count. */
const asWords = (s: string) => new RegExp(`(?<![\\p{L}\\d])${s.replace(/[.*+?^$()|[\]\\]/g, "\\$&")}(?![\\p{L}\\d])`, "u");

async function expectNoEnglish(region: Locator) {
  const text = await region.innerText();
  expect(ENGLISH.filter((s) => asWords(s).test(text))).toEqual([]);
}

test("the shell and the dashboard are German with the German catalog", async ({ page, request }, testInfo) => {
  await resetUiSettings(request); // the built-in panels, not a customized dashboard
  await forceGerman(page);
  await page.goto("/");
  await expect(page).toHaveTitle(/^Dashboard · /);
  await expect(page.getByRole("heading", { level: 1, name: /^Dashboard: Guten (Morgen|Tag|Abend)$/ })).toBeVisible();
  await expect(page.getByRole("radiogroup", { name: "Zeitraum" }).getByRole("radio", { name: "14 Tage" })).toBeChecked();
  await expect(page.getByRole("heading", { name: "CIs nach Klasse", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Letzte Aktivität", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Änderungen der letzten 14 Tage", exact: true })).toBeVisible();
  const stats = page.getByRole("region", { name: "Kennzahlen" });
  await expect(stats.locator('[data-stat="total"] .label')).toHaveText("Configuration Items");
  await expect(stats.locator('[data-stat="relationships"] .label')).toHaveText("Beziehungen");
  await expect(stats.locator('[data-stat="changes"] .label')).toHaveText("Änderungen im Zeitraum");
  await expect(stats.locator('[data-stat="complete"] .label')).toHaveText("Vollständige Datensätze");
  await expect(page.getByRole("columnheader", { name: "Änderung" })).toBeVisible();
  await expect(page.locator(".count-value .spinner")).toHaveCount(0); // every count has loaded
  await expect(page.locator(".count-value").first()).toHaveAttribute("title", /^(<1|\d+) % aller CIs$/);

  const nav = page.getByRole("navigation", { name: "Hauptmenü" });
  const inventory = nav.getByRole("link", { name: "Alle CIs", exact: true });
  await expect(inventory).toBeVisible();
  // GH#888: at 1440 px with the count badge shown, the German label fits without an ellipsis.
  await expect(inventory.locator(".nav-count")).toHaveText(/\S/);
  expect(await inventory.locator(".nav-label").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await expect(page.getByRole("navigation", { name: "Navigationspfad" })).toBeVisible();
  // In the banner: the dashboard's own "Neues CI" button would otherwise match too.
  await expect(page.getByRole("banner").getByRole("link", { name: "Neues CI", exact: true })).toBeVisible();
  await openUserMenu(page);
  await expect(page.getByRole("button", { name: "Abmelden", exact: true })).toBeVisible();
  await expect(page.getByLabel("Dichte")).toBeVisible();
  await expect(page.getByLabel("Configuration Items durchsuchen")).toHaveAttribute("placeholder", /^CIs nach Bezeichnung/);
  await expectNoEnglish(page.locator("body"));
  await checkA11y(page, testInfo, "dashboard-de");

  // The search's type-ahead note.
  await page.getByLabel("Configuration Items durchsuchen").fill("zz-no-such-ci");
  await expect(page.locator("#global-search-list")).toHaveText("Kein CI passt zu „zz-no-such-ci“");
});

test("not found, the pagination bar and the error alert are German with the German catalog", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/no/such/screen");
  await expect(page.getByRole("heading", { name: "Seite nicht gefunden" })).toBeVisible();
  await expect(page).toHaveTitle(/^Nicht gefunden · /);
  await expect(page.getByRole("link", { name: "Zum Dashboard" })).toBeVisible();
  await expectNoEnglish(page.locator("main"));

  await page.goto("/cis");
  const pagination = page.locator(".pagination");
  await expect(pagination).toContainText(/^1–\d+ von [\d.]+/);
  await expect(pagination.getByRole("navigation", { name: "Seiten" }).getByRole("button", { name: "Seite 1" })).toHaveAttribute("aria-current", "page");
  await expect(pagination.getByLabel("Zeilen")).toBeVisible();
  await expect(pagination.getByRole("button", { name: "Nächste Seite" })).toBeVisible();
  await expectNoEnglish(pagination);

  await page.route("**/api/v1/**", (route) => route.abort("connectionrefused"));
  await page.reload();
  const card = page.getByTestId("boot-error");
  await expect(card.getByRole("heading", { level: 1, name: "API nicht erreichbar" })).toBeVisible({ timeout: 15_000 });
  await expect(card.getByRole("alert")).toContainText("Die ShadouCMDB-API unter");
  await expect(card.getByRole("button", { name: "Erneut versuchen" })).toBeVisible();
  await expectNoEnglish(card);
});

test("the inventory's built-in column headers and the Columns popover are German with the German catalog (SHAA-2406)", async ({ page, request }) => {
  await resetUiSettings(request); // the default columns, not a list view's
  await forceGerman(page);
  await page.goto("/cis");
  const headers = page.locator("table.data thead th:not(.row-actions):not(.select-cell)");
  await expect(headers).toHaveText([/^\s*Bezeichnung/, /^\s*Ident/, /^\s*Klasse/, /^\s*Aktiv/, /^\s*Geändert/]);
  await expect(page.getByRole("columnheader", { name: /^Label|^Class|^Active|^Updated/ })).toHaveCount(0);

  await page.getByRole("button", { name: /^Spalten/ }).click();
  const popover = page.getByRole("dialog", { name: "Spalten" });
  await expect(popover.getByRole("list", { name: "Angezeigte Spalten" }).getByRole("listitem")).toHaveText([
    /^\s*Bezeichnung/,
    /^\s*Ident/,
    /^\s*Klasse/,
    /^\s*Aktiv/,
    /^\s*Geändert/,
  ]);
  const more = popover.getByRole("group", { name: "Weitere Felder" });
  for (const name of ["Kritikalität", "Gültig ab", "Gültig bis", "Angelegt"]) await expect(more.getByLabel(name, { exact: true })).toBeVisible();
});
