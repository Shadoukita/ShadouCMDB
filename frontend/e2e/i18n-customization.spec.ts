import type { Locator, Page } from "@playwright/test";
import { de } from "../src/i18n/de";
import { en, type MessageKey } from "../src/i18n/en";
import { expect, resetUiSettings, test } from "./support";

// Administration › Customization takes every text of its Navigation, Dashboard, List views and History
// sections from the message catalog (SHAA-2661). With the test-only German locale forced, none of their
// English texts may show. The sections are only opened and edited as a draft: nothing is saved.

test.describe.configure({ mode: "serial" });
test.beforeAll(async ({ request }) => resetUiSettings(request));

/** This slice's English texts that read differently in German (messages with parameters are checked by the assertions). */
const ENGLISH = (Object.keys(en) as MessageKey[])
  .filter((k) => k.startsWith("customization.") && en[k] !== de[k] && !en[k].includes("{") && en[k].length >= 4)
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

test("customization: the Navigation section is German with the German catalog", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/admin/customization/navigation");
  const editor = page.locator(".nav-editor");
  await expect(editor.getByRole("heading", { name: "Hauptmenü", exact: true })).toBeVisible();
  for (const name of ["Eintrag", "Angezeigt als", "Sichtbar"]) await expect(editor.getByRole("columnheader", { name, exact: true })).toBeVisible();
  const table = editor.locator("table");
  await expect(table.getByLabel("Name für Alle CIs")).toBeVisible();
  await expect(table.getByRole("button", { name: "Alle CIs nach unten verschieben" })).toBeVisible();
  // A section brings the "Into section…" choice and its Remove button.
  await page.getByLabel("Neuer Abschnitt").fill("Rechenzentrum");
  await page.getByRole("button", { name: "Abschnitt hinzufügen" }).click();
  const into = table.getByLabel("Server in einen Abschnitt verschieben");
  await expect(into.locator("option").first()).toHaveText("In Abschnitt…");
  await expect(table.getByRole("button", { name: "Abschnitt entfernen" })).toBeVisible();
  await into.selectOption({ label: "Rechenzentrum" });
  await expect(table.getByRole("button", { name: "Aus dem Abschnitt" })).toBeVisible();
  await expect(editor.getByRole("link", { name: "Administration › Bereiche" })).toBeVisible();
  await expectNoEnglish(editor);
});

test("customization: the Dashboard section is German with the German catalog", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/admin/customization/dashboard");
  await expect(page.getByRole("radiogroup", { name: "Inhalt des Dashboards" })).toBeVisible();
  await page.getByRole("radio", { name: "Widgets auswählen" }).check();
  for (const name of ["Widget", "Titel", "Breite", "Optionen"]) await expect(page.getByRole("columnheader", { name, exact: true })).toBeVisible();
  await page.getByLabel("Widget hinzufügen").selectOption("saved_search");
  await page.getByRole("button", { name: "Hinzufügen", exact: true }).click();
  const row = page.locator("tr", { has: page.locator("code", { hasText: /^saved_search$/ }) });
  await expect(row.getByLabel("Titel von saved_search")).toBeVisible();
  await expect(row.getByRole("group", { name: "Klassen" }).first()).toContainText("Nichts angekreuzt: jede Klasse");
  await expect(row.getByLabel("Unterklassen einbeziehen")).toBeVisible();
  await expect(row.getByRole("button", { name: "saved_search nach oben verschieben" })).toBeVisible();
  await expect(row.getByRole("button", { name: "saved_search entfernen" })).toHaveText("Entfernen");
  await expect(page.getByLabel("Vorschau des Dashboards")).toContainText("Vorschau mit echten Daten");
  await expectNoEnglish(page.locator("section.panel").first());

  // Every widget removed: the empty text.
  while ((await page.getByRole("button", { name: /entfernen$/ }).count()) > 0) await page.getByRole("button", { name: /entfernen$/ }).first().click();
  await expect(page.getByText("Keine Widgets: Das Dashboard bleibt leer. Fügen Sie unten eines hinzu.")).toBeVisible();
});

test("customization: the List views section is German with the German catalog", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/admin/customization/list-views");
  const panel = page.locator("section.panel").first();
  await expect(panel.getByRole("heading", { name: "Listenansichten", exact: true })).toBeVisible();
  await expect(panel).toContainText("Das Inventar einer Klasse: Spalten, Sortierung, Filter, Seitengröße");
  await expect(panel).toContainText("0 Klassen haben eine eigene Listenansicht");
  await expect(panel.getByLabel("Klasse", { exact: true }).locator("option").first()).toHaveText("Klasse wählen…");
  await panel.getByLabel("Klasse", { exact: true }).selectOption("server");
  await expect(panel).toContainText("Server verwendet die Standardliste");
  await panel.getByRole("button", { name: "Liste von Server anpassen" }).click();
  await expect(panel.getByRole("list", { name: "Spalten" })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Bezeichnung nach unten verschieben" })).toBeVisible();
  await expect(panel.getByLabel("Standardsortierung")).toBeVisible();
  await expect(panel.getByLabel("Zeilen pro Seite (10-200)")).toBeVisible();
  await expect(panel.getByRole("button", { name: "Standardliste für Server verwenden" })).toBeVisible();
  await expect(page.getByLabel("Vorschau der Liste")).toContainText("Vorschau: die ersten CIs von Server mit diesen Spalten");
  await expectNoEnglish(panel);
});

test("customization: the History section is German with the German catalog", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/admin/customization/history");
  const panel = page.locator("section.panel").first();
  await expect(panel.getByRole("heading", { name: "Gespeicherte Versionen", exact: true })).toBeVisible();
  await expect(panel).toContainText("Neueste zuerst");
  for (const name of ["Version", "Gespeichert", "Von", "Kommentar"]) await expect(panel.getByRole("columnheader", { name, exact: true })).toBeVisible();
  const current = panel.locator("tbody tr").first();
  await expect(current.locator(".badge.ok")).toHaveText("aktuell");
  await current.getByRole("button", { name: "Anzeigen" }).click();
  await expect(current.getByRole("button", { name: "Ausblenden" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /^Version \d+ wie gespeichert$/ })).toBeVisible();
  // An older version offers Restore, and its dialog is German too (cancelled: nothing is restored).
  const older = panel.locator("tbody tr").nth(1);
  if ((await older.count()) > 0) {
    await older.getByRole("button", { name: "Wiederherstellen" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("heading", { name: /^Version \d+ wiederherstellen\?$/ })).toBeVisible();
    await expect(dialog).toContainText("bleibt im Verlauf");
    await dialog.getByRole("button", { name: "Abbrechen" }).click();
  }
});
