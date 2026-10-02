import type { Page } from "@playwright/test";
import { de } from "../src/i18n/de";
import { en, type MessageKey } from "../src/i18n/en";
import { expect, test } from "./support";

// The sign-in and account screens take every text from the message catalog (SHAA-1415). With the
// test-only German locale forced, none of their English texts may show: a leftover hard-coded string
// or a missing `t()` would. The UI itself stays English; there is no setting for this.

/** This slice's English texts that read differently in German (messages with parameters are checked by the headings). */
const ENGLISH = (Object.keys(en) as MessageKey[])
  .filter((k) => /^(auth|account)\./.test(k) && en[k] !== de[k] && !en[k].includes("{") && en[k].length >= 4)
  .map((k) => en[k]);

async function forceGerman(page: Page) {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
}

/** Whole words only: "Password" is English, the "1Password" app in the German text is not. */
const asWords = (s: string) => new RegExp(`(?<![\\p{L}\\d])${s.replace(/[.*+?^$()|[\]\\]/g, "\\$&")}(?![\\p{L}\\d])`, "u");

async function expectNoEnglish(page: Page) {
  const text = await page.locator("main").innerText();
  expect(ENGLISH.filter((s) => asWords(s).test(text))).toEqual([]);
}

test.describe("signed out", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test("sign-in is German with the German catalog", async ({ page }) => {
    await forceGerman(page);
    await page.goto("/login");
    await expect(page.getByRole("heading", { level: 1, name: "Anmelden" })).toBeVisible();
    await expect(page).toHaveTitle(/^Anmelden · /);
    await expect(page.getByLabel("Benutzername")).toBeFocused();
    await expect(page.getByLabel("Passwort")).toBeVisible();
    await expect(page.getByRole("button", { name: "Anmelden", exact: true })).toBeVisible();
    await expect(page.locator("main")).toContainText("Kein Zugriff mehr auf ein Administratorkonto? Führen Sie shadoucmdb create-admin auf dem Server aus.");
    await expectNoEnglish(page);
  });
});

test("My account is German with the German catalog, including the checks next to the fields", async ({ page }) => {
  await forceGerman(page);
  await page.goto("/account");
  await expect(page.getByRole("heading", { level: 1, name: "Mein Konto" })).toBeVisible();
  await expect(page).toHaveTitle(/^Mein Konto · /);
  const password = page.getByRole("region", { name: "Passwort", exact: true });
  await expect(password.getByLabel("Aktuelles Passwort")).toBeVisible();
  const mfa = page.getByRole("region", { name: "Zwei-Faktor-Authentifizierung", exact: true });
  await expect(mfa.locator(".badges")).toBeVisible();
  await expect(mfa.locator(".badges .badge").first()).toHaveText(/^(Ein|Aus)$/);

  await password.getByRole("button", { name: "Passwort ändern" }).click();
  await expect(page.locator("#own-current-password-err")).toHaveText("Pflichtfeld");
  await expect(page.locator("#own-new-password-err")).toHaveText("Zu kurz");
  await expectNoEnglish(page);
});
