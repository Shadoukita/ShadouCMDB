import type { Page } from "@playwright/test";
import { de } from "../src/i18n/de";
import { en, type MessageKey } from "../src/i18n/en";
import { classIdByName, createCi, expect, snap, test } from "./support";

// The core screens take every text from the message catalog (design document §1.12 N1, step 11-4b): the CI record,
// its Impact tab, the create and edit pages, and the layout editor. With the test-only German locale forced, none of
// their English texts may show, neither as text nor as an accessible name or tooltip.

/** Names that come from the server, in English in the demo data: the template's "Notes" attribute, the built-in class. */
const SERVER_NAMES = new Set(["Notes", "Business service"]);
/** This slice's English texts that read differently in German (messages with parameters are checked by name). */
const ENGLISH = (Object.keys(en) as MessageKey[])
  .filter((k) => /^(impact|record|form|layoutEditor|clone)\./.test(k) && en[k] !== de[k] && !en[k].includes("{") && en[k].length >= 5)
  .map((k) => en[k])
  .filter((s) => !SERVER_NAMES.has(s));

async function forceGerman(page: Page, opts: { theme?: "dark"; density?: "comfortable" } = {}) {
  await page.addInitScript((o) => {
    (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de";
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
  }, opts);
}

const asWords = (s: string) => new RegExp(`(?<![\\p{L}\\d])${s.replace(/[.*+?^$()|[\]\\]/g, "\\$&")}(?![\\p{L}\\d])`, "u");

/** The visible text of `main`, plus the accessible names and tooltips of its elements. */
async function shown(page: Page): Promise<string> {
  const main = page.locator("main");
  const attrs = await main.evaluate((el) =>
    [...el.querySelectorAll("[aria-label], [title], [placeholder]")]
      .flatMap((e) => ["aria-label", "title", "placeholder"].map((a) => e.getAttribute(a) ?? ""))
      .join("\n"),
  );
  return `${await main.innerText()}\n${attrs}`;
}

async function expectNoEnglish(page: Page) {
  const text = await shown(page);
  expect(ENGLISH.filter((s) => asWords(s).test(text))).toEqual([]);
}

const stamp = Date.now().toString(36);
let ci = { id: "", label: "" };
let serverId = "";

test.beforeAll(async ({ request }) => {
  serverId = await classIdByName(request, "Server");
  ci = await createCi(request, serverId, `i18n-core-${stamp}`);
});

test("the CI record and its Impact tab are German", async ({ page }) => {
  await forceGerman(page);
  await page.goto(`/cis/${ci.id}`);
  await expect(page.getByRole("heading", { level: 1, name: ci.label })).toBeVisible();
  await expect(page.locator("main")).toContainText("Allgemein");
  await expectNoEnglish(page);

  // The delete confirmation keeps the class name as it is, and says what happens, in German.
  await page.getByRole("button", { name: "Weitere Aktionen" }).click();
  await page.getByRole("menuitem", { name: "Löschen" }).click();
  const dialog = page.getByRole("dialog", { name: `Server „${ci.label}“ löschen?` });
  await expect(dialog).toContainText("wird aus dem Inventar entfernt");
  await expect(dialog).toContainText("Es hat keine Beziehungen, daher ist kein anderes CI betroffen.");
  await expect(dialog.getByRole("button", { name: "CI löschen" })).toBeVisible();
  const text = await dialog.innerText();
  expect(ENGLISH.filter((s) => asWords(s).test(text))).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);

  await page.goto(`/cis/${ci.id}/impact`);
  await expect(page.getByRole("radiogroup", { name: "Richtung" })).toBeVisible();
  await expect(page.locator(".impact-summary, .empty-state").first()).toBeVisible();
  await expectNoEnglish(page);
});

test("the create and edit pages are German", async ({ page }) => {
  await forceGerman(page);
  await page.goto(`/cis/new?classId=${serverId}`);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Server");
  await expect(page.locator("main form")).toBeVisible();
  await expectNoEnglish(page);

  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("main form")).toBeVisible();
  await expect(page.locator("#f-ident")).toHaveValue(/.+/);
  await expectNoEnglish(page);
});

test("the layout editor is German: the bar, the field toolbar and the windows", async ({ page }) => {
  await forceGerman(page);
  await page.goto(`/cis/${ci.id}/layout-editor`);
  const editing = page.getByRole("region", { name: "Layout bearbeiten" });
  await expect(editing).toBeVisible();
  // A field's grip names it in German, and its toolbar offers the moves in German.
  const grip = page.locator(".le-grip").first();
  await expect(grip).toHaveAttribute("aria-label", /von \d+ Spalten|vom Layout nicht platziert/);
  await expect(grip).toHaveAttribute("title", "Zum Verschieben ziehen");
  await grip.focus();
  await expect(page.locator(".le-field").first().getByRole("toolbar")).toHaveAttribute("aria-label", /: Layout$/);
  await expect(page.locator(".le-field").first().getByRole("button", { name: /ausblenden$/ })).toBeVisible();
  await expect(page.locator("[data-testid='window-edge-se']").first()).toHaveAttribute("title", "Zum Ändern der Größe ziehen");
  await expectNoEnglish(page);
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): both themes, both densities, en and de, 1440 and 900 px.
const SCREENS = [
  { name: "record", path: () => `/cis/${ci.id}` },
  { name: "impact", path: () => `/cis/${ci.id}/impact` },
  { name: "create", path: () => `/cis/new?classId=${serverId}` },
  { name: "layout-editor", path: () => `/cis/${ci.id}/layout-editor` },
];
for (const screen of SCREENS)
  for (const theme of ["light", "dark"] as const)
    for (const density of ["compact", "comfortable"] as const)
      for (const locale of ["en", "de"] as const)
        for (const width of [1440, 900])
          test(`core screens screenshots: ${screen.name}, ${theme}, ${density}, ${locale}, ${width}`, async ({ page }) => {
            test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
            await page.setViewportSize({ width, height: 900 });
            await page.addInitScript((o) => {
              if (o.locale === "de") (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de";
              if (o.theme === "dark") localStorage.setItem("shadoucmdb.theme", "dark");
              if (o.density === "comfortable") localStorage.setItem("shadoucmdb.density", "comfortable");
            }, { theme, density, locale });
            await page.goto(screen.path());
            await expect(page.locator("main h1, main [role='region']").first()).toBeVisible();
            await page.waitForLoadState("networkidle");
            await snap(page, `core-${screen.name}-${locale}-${theme}-${density}-${width}`);
          });
