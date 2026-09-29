import type { Browser, Page } from "@playwright/test";
import { createHmac } from "node:crypto";
import { apiGet, apiSend, at, expect, snap, test } from "./support";

// Two-factor authentication, in order: an operator sets up an authenticator (QR code, recovery codes), signs in
// with a code and with a recovery code, replaces the codes and turns it off; then an administrator makes it
// mandatory on a profile, a holder is sent straight to the set-up, and the administrator resets it.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = `mfa-e2e-password-${stamp}`;

// ---------- RFC 6238 TOTP, as an authenticator app computes it ----------

function base32(secret: string): Buffer {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const ch of secret.replace(/[\s=]/g, "").toUpperCase()) bits += alphabet.indexOf(ch).toString(2).padStart(5, "0");
  const bytes: number[] = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) bytes.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(bytes);
}

function totpAt(secret: string, step: number): string {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const mac = createHmac("sha1", base32(secret)).update(counter).digest();
  const offset = mac[mac.length - 1] & 0x0f;
  return ((mac.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).toString().padStart(6, "0");
}

/**
 * The API refuses a code it has already accepted and allows one 30 s step of drift either way, so each
 * use takes a fresh step from (now - 1, now, now + 1). A code for step k is accepted until step k + 1 ends,
 * and the API checks it only after the password (an argon2id hash, seconds on a busy host), so a code is
 * handed out only if it stays valid for at least VALID_FOR ms: now - 1 only early in a step. When the
 * steps up to now + 1 are used up, next() waits for the next step (up to 30 s; callers mark the test slow).
 */
const VALID_FOR = 20_000;
function authenticator(secret: string) {
  let last = -Infinity;
  return {
    async next(): Promise<string> {
      for (;;) {
        const ms = Date.now();
        const now = Math.floor(ms / 30_000);
        let step = Math.max(now - 1, last + 1);
        if ((step + 2) * 30_000 - ms < VALID_FOR) step += 1;
        if (step <= now + 1) {
          last = step;
          return totpAt(secret, step);
        }
        await new Promise((r) => setTimeout(r, (now + 1) * 30_000 - ms + 100));
      }
    },
  };
}

// ---------- helpers ----------

// Sign-in and every password re-check hash with argon2id, which takes seconds on a busy host; the first
// assertion after one gets longer than the default 5 s (as in account-password.spec.ts).
const ARGON2 = { timeout: 20_000 };

interface User {
  id: string;
  username: string;
  mfaEnabled: boolean;
}
interface Profile {
  id: string;
  name: string;
  isBuiltin: boolean;
  requireMfa: boolean;
}

async function newPage(browser: Browser): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] }, acceptDownloads: true });
  return context.newPage();
}

async function signInWithPassword(page: Page, username: string) {
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
}

async function signOut(page: Page) {
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(at("/login"));
}

/** Reads the recovery codes off the screen, checks the download, and confirms they were saved. */
async function saveRecoveryCodes(page: Page): Promise<string[]> {
  const list = page.getByRole("list", { name: "Recovery codes" });
  // New recovery codes follow a password check.
  await expect(list.getByRole("listitem")).toHaveCount(10, ARGON2);
  const codes = await list.getByRole("listitem").allInnerTexts();
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download .txt" }).click()]);
  expect(download.suggestedFilename()).toMatch(/^shadoucmdb-recovery-codes-.+\.txt$/);
  const done = page.getByRole("button", { name: "Done" });
  await expect(done).toBeDisabled();
  await page.getByLabel("I have saved these recovery codes").check();
  await done.click();
  return codes;
}

/** Password → QR code and setup key → first code → recovery codes. Returns the key and the codes. */
async function enrol(page: Page, shot: string) {
  await page.locator("#mfa-currentPassword").fill(PASSWORD);
  await page.getByRole("button", { name: "Set up authenticator app" }).click();
  await expect(page.getByRole("img", { name: "QR code to add ShadouCMDB to your authenticator app" })).toBeVisible(ARGON2);
  const secret = await page.getByLabel("Setup key").inputValue();
  expect(secret.replace(/\s/g, "")).toMatch(/^[A-Z2-7]{16,}$/);
  await snap(page, `${shot}-qr`);

  // A wrong code is refused next to the field; the set-up stays open.
  await page.getByLabel("Code from the app").fill("000000");
  await page.getByRole("button", { name: "Verify and turn on" }).click();
  await expect(page.locator("#mfa-code-err")).toBeVisible();

  const app = authenticator(secret);
  await page.getByLabel("Code from the app").fill(await app.next());
  await page.getByRole("button", { name: "Verify and turn on" }).click();
  await expect(page.getByText("Save these codes now. You won't see them again.")).toBeVisible();
  await snap(page, `${shot}-recovery-codes`);
  const codes = await saveRecoveryCodes(page);
  return { app, codes };
}

// ---------- self-service ----------

test.describe("an operator's own two-factor authentication", () => {
  const USERNAME = `e2e-mfa-${stamp}`;
  let page: Page;
  let app: ReturnType<typeof authenticator>;
  let codes: string[] = [];

  test.beforeAll(async ({ request, browser }) => {
    await apiSend(request, "POST", "/admin/users", { username: USERNAME, displayName: `MFA operator ${stamp}`, password: PASSWORD, profileIds: [] });
    page = await newPage(browser);
  });
  test.afterAll(async () => page.context().close());

  test("set up an authenticator app from My account: QR code, setup key, recovery codes", async () => {
    test.slow(); // three argon2id checks: sign-in, a wrong password, the set-up
    await signInWithPassword(page, USERNAME);
    await expect(page).toHaveURL(at("/"), ARGON2);
    await page.getByRole("link", { name: /MFA operator/ }).click();
    await expect(page).toHaveURL(at("/account"));
    const panel = page.getByRole("region", { name: "Two-factor authentication" });
    await expect(panel.getByText("Off", { exact: true })).toBeVisible();

    // The password is checked first, next to its field.
    await page.locator("#mfa-currentPassword").fill("not-the-password");
    await page.getByRole("button", { name: "Set up authenticator app" }).click();
    await expect(page.locator("#mfa-currentPassword-err")).toBeVisible(ARGON2);

    ({ app, codes } = await enrol(page, "mfa-self"));
    await expect(page.getByRole("status").filter({ hasText: "Two-factor authentication is on." })).toBeVisible();
    await expect(panel.getByText("On", { exact: true })).toBeVisible();
    await expect(panel.getByText("10 unused")).toBeVisible();
    await snap(page, "mfa-self-on");
  });

  test("sign-in asks for a code after the password; a wrong code is refused", async () => {
    await signOut(page);
    await signInWithPassword(page, USERNAME);
    await expect(page.getByRole("heading", { name: "Two-factor authentication" })).toBeVisible(ARGON2);
    await expect(page.getByLabel("Authentication code")).toBeFocused();
    await snap(page, "mfa-login-code");

    await page.getByLabel("Authentication code").fill("000000");
    await page.getByRole("button", { name: "Verify" }).click();
    await expect(page.getByRole("alert")).toContainText("Wrong code");

    await page.getByLabel("Authentication code").fill(await app.next());
    await page.getByRole("button", { name: "Verify" }).click();
    await expect(page).toHaveURL(at("/"));
  });

  test("a recovery code signs in once in place of a code", async () => {
    await signOut(page);
    await signInWithPassword(page, USERNAME);
    const lost = page.getByRole("button", { name: "Lost your device? Use a recovery code" });
    await expect(lost).toBeVisible(ARGON2);
    await lost.click();
    await page.getByLabel("Recovery code").fill(codes[0]);
    await page.getByRole("button", { name: "Verify" }).click();
    await expect(page).toHaveURL(at("/"));

    await page.goto("/account");
    await expect(page.getByRole("region", { name: "Two-factor authentication" }).getByText("9 unused")).toBeVisible();
  });

  test("replace the recovery codes, then turn two-factor authentication off", async () => {
    // Three argon2id checks (new codes, turn off, sign-in), and the second code may wait for the next 30 s step.
    test.slow();
    await page.getByRole("button", { name: "New recovery codes" }).click();
    await page.locator("#mfa-currentPassword").fill(PASSWORD);
    await page.getByLabel("Authentication code").fill(await app.next());
    await page.getByRole("button", { name: "Create new recovery codes" }).click();
    const fresh = await saveRecoveryCodes(page);
    expect(fresh).not.toContain(codes[1]);
    const panel = page.getByRole("region", { name: "Two-factor authentication" });
    await expect(panel.getByText("10 unused")).toBeVisible();

    await page.getByRole("button", { name: "Turn off" }).click();
    await page.locator("#mfa-currentPassword").fill(PASSWORD);
    await page.getByLabel("Authentication code").fill(await app.next());
    await page.getByRole("button", { name: "Turn off two-factor authentication" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Sign-in asks for your password only." })).toBeVisible(ARGON2);
    await expect(panel.getByText("Off", { exact: true })).toBeVisible();

    await signOut(page);
    await signInWithPassword(page, USERNAME);
    await expect(page).toHaveURL(at("/"), ARGON2);
  });
});

// ---------- administration ----------

test.describe("administrators: required two-factor authentication and reset", () => {
  const USERNAME = `e2e-mfa-required-${stamp}`;
  const PROFILE = `E2E two-factor required ${stamp}`;
  let profile: Profile;
  let user: User;

  test.beforeAll(async ({ request }) => {
    profile = await apiSend<Profile>(request, "POST", "/admin/profiles", { name: PROFILE, globalPermissions: [], classPermissions: [] });
    user = await apiSend<User>(request, "POST", "/admin/users", {
      username: USERNAME,
      displayName: `MFA required ${stamp}`,
      password: PASSWORD,
      profileIds: [profile.id],
    });
  });

  test("the built-in Administrator profile accepts only the two-factor requirement", async ({ page }) => {
    const admin = (await apiGet<{ data: Profile[] }>(page.request, "/admin/profiles?limit=200")).data.find((p) => p.isBuiltin)!;
    await page.goto(`/admin/profiles/${admin.id}`);
    await expect(page.locator("#profile-name")).toHaveJSProperty("readOnly", true);
    await expect(page.getByRole("checkbox", { name: /^view on all classes$/i })).toBeDisabled();

    // Saving sends only requireMfa. The answer is stubbed: really turning it on would send every
    // administrator of a shared e2e database (this one included) to the two-factor set-up.
    let sent: unknown;
    await page.route(`**/api/v1/admin/profiles/${admin.id}`, async (route) => {
      if (route.request().method() !== "PATCH") return route.fallback();
      sent = route.request().postDataJSON();
      await route.fulfill({ json: { ...admin, requireMfa: true } });
    });
    await page.getByLabel("Require two-factor authentication").check();
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByRole("status").filter({ hasText: `Saved ${admin.name}.` })).toBeVisible();
    expect(sent).toEqual({ requireMfa: true });
  });

  test("require two-factor authentication on a profile", async ({ page, request }) => {
    await page.goto(`/admin/profiles/${profile.id}`);
    const toggle = page.getByLabel("Require two-factor authentication");
    await expect(toggle).not.toBeChecked();
    await toggle.check();
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByRole("status").filter({ hasText: `Saved ${PROFILE}.` })).toBeVisible();
    expect((await apiGet<Profile>(request, `/admin/profiles/${profile.id}`)).requireMfa).toBe(true);

    await page.goto(`/admin/profiles?q=${encodeURIComponent(PROFILE)}`);
    await expect(page.getByRole("row", { name: new RegExp(PROFILE) }).getByText("Two-factor required", { exact: true })).toBeVisible();
  });

  test("a holder without two-factor is sent straight to the set-up, then into the app", async ({ browser }) => {
    test.slow(); // two argon2id checks: sign-in and the set-up
    const page = await newPage(browser);
    await page.goto("/cis");
    await page.getByLabel("Username").fill(USERNAME);
    await page.getByLabel("Password").fill(PASSWORD);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page).toHaveURL(at("/two-factor-setup", "?redirect=/cis"), ARGON2);
    await expect(page.getByRole("heading", { level: 1, name: "Set up two-factor authentication" })).toBeVisible();
    await expect(page.getByText("A permission profile you hold requires two-factor authentication.")).toBeVisible();
    // No way around it: the app shell is not there, and any other address comes back here.
    await expect(page.getByRole("navigation", { name: "Main" })).toHaveCount(0);
    await page.goto("/account");
    await expect(page).toHaveURL(at("/two-factor-setup", "?redirect=/account"));
    await snap(page, "mfa-forced-setup");

    await enrol(page, "mfa-forced");
    await expect(page).toHaveURL(at("/account"));
    await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
    const panel = page.getByRole("region", { name: "Two-factor authentication" });
    await expect(panel.getByText("On", { exact: true })).toBeVisible();
    await expect(panel.getByText("Required", { exact: true })).toBeVisible();
    await page.context().close();
  });

  test("user management shows who has two-factor and resets it", async ({ page, request }) => {
    await page.goto(`/admin/users?q=${USERNAME}`);
    const row = page.getByRole("row", { name: new RegExp(USERNAME) });
    await expect(row.getByText("On", { exact: true })).toBeVisible();
    await row.getByRole("link", { name: USERNAME }).click();

    await expect(page.getByText("Two-factor on")).toBeVisible();
    await page.getByRole("button", { name: "Reset two-factor" }).click();
    const dialog = page.getByRole("dialog", { name: `Reset two-factor authentication for ${USERNAME}?` });
    await expect(dialog).toContainText("must set it up again");
    await snap(page, "mfa-admin-reset");
    await dialog.getByRole("button", { name: "Reset two-factor" }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByRole("status").filter({ hasText: "Two-factor authentication reset." })).toBeVisible();
    await expect(page.getByRole("button", { name: "Reset two-factor" })).toBeDisabled();
    expect((await apiGet<User>(request, `/admin/users/${user.id}`)).mfaEnabled).toBe(false);
  });
});
