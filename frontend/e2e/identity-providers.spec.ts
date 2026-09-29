import type { APIRequestContext, Browser, Page, Route } from "@playwright/test";
import { apiGet, apiSend, expect, snap, test } from "./support";

// Enterprise sign-in: the sign-in page's "Sign in with …" buttons and error messages, and
// Administration › Identity providers (OIDC and LDAP settings, write-only secrets, group mappings,
// the connection test, disable/delete) against the live API. There is no real identity provider in
// the test environment, so the settings point at a closed local port: the connection test fails,
// and the directory is created disabled so that other specs' password sign-ins never ask it.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const OIDC_NAME = `E2E Entra ${stamp}`;
const LDAP_NAME = `E2E Directory ${stamp}`;
const SECRET = `e2e-client-secret-${stamp}`;
const SECRET_2 = `e2e-client-secret-2-${stamp}`;
const BIND_PASSWORD = `e2e-bind-password-${stamp}`;
const BIND_PASSWORD_2 = `e2e-bind-password-2-${stamp}`;
const MANAGER = `e2e-user-manager-${stamp}`;
const MANAGER_PASSWORD = "user-manager-password-123";

interface Provider {
  id: string;
  name: string;
  isEnabled: boolean;
  userCount: number;
  oidc: { clientSecretSet: boolean; redirectUri: string | null; mfaAssurance: "verify" | "trustProvider"; requiredAcr: string[] } | null;
  ldap: { bindPasswordSet: boolean; startTls: boolean } | null;
  groupMappings: { group: string; profileName: string }[];
}

let oidcId = "";
let ldapId = "";
let managerProfileId = "";
let managerId = "";

const SSO_CODES: Record<string, RegExp> = {
  expired: /took too long/,
  cancelled: /cancelled at the identity provider/,
  failed: /could not be verified/,
  unavailable: /could not be reached/,
  not_configured: /PUBLIC_URL is missing/,
  not_authorised: /None of your groups gives access/,
  account_conflict: /already exists/,
  account_disabled: /account is disabled/,
  invalid_username: /did not send a usable username/,
  last_administrator: /without an active administrator/,
  mfa_not_enforced: /did not confirm a second factor, which your access to ShadouCMDB requires/,
};

/** A signed-out page. */
async function anonymousPage(browser: Browser): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  return context.newPage();
}

/** Answers GET /auth/providers with `body` (the environment has no real enabled OIDC provider). */
async function mockProviders(page: Page, body: { oidc: { id: string; name: string; startUrl: string }[]; directory: boolean }) {
  await page.route("**/api/v1/auth/providers", (route: Route) => route.fulfill({ json: body }));
}

async function provider(request: APIRequestContext, id: string): Promise<Provider> {
  return apiGet<Provider>(request, `/admin/identity-providers/${id}`);
}

test.beforeAll(async ({ request }) => {
  managerProfileId = (
    await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: `E2E user managers ${stamp}`,
      globalPermissions: ["users.manage", "profiles.manage"],
      classPermissions: [],
    })
  ).id;
  managerId = (
    await apiSend<{ id: string }>(request, "POST", "/admin/users", {
      username: MANAGER,
      displayName: `E2E User Manager ${stamp}`,
      password: MANAGER_PASSWORD,
      profileIds: [managerProfileId],
    })
  ).id;
});

test.afterAll(async ({ request }) => {
  const csrf = (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
  for (const id of [oidcId, ldapId]) {
    if (id) await request.delete(`/api/v1/admin/identity-providers/${id}`, { headers: { "X-CSRF-Token": csrf } });
  }
  if (managerId) await request.delete(`/api/v1/admin/users/${managerId}`, { headers: { "X-CSRF-Token": csrf } });
  if (managerProfileId) await request.delete(`/api/v1/admin/profiles/${managerProfileId}`, { headers: { "X-CSRF-Token": csrf } });
});

// ---------- Sign-in page ----------

test("sign-in page: one button per OIDC provider, a real link that carries the return path, and the directory hint", async ({ browser }) => {
  const page = await anonymousPage(browser);
  const unknown = "00000000-0000-4000-8000-00000000e2e0";
  await mockProviders(page, {
    oidc: [
      { id: unknown, name: "Contoso Entra ID", startUrl: `/api/v1/auth/oidc/${unknown}/start` },
      { id: "00000000-0000-4000-8000-00000000e2e1", name: "Keycloak", startUrl: "/api/v1/auth/oidc/00000000-0000-4000-8000-00000000e2e1/start" },
    ],
    directory: true,
  });
  await page.goto("/cis?q=crm");
  await expect(page).toHaveURL(/\/login\?redirect=/);
  const sso = page.getByRole("navigation", { name: "Single sign-on" });
  const entra = sso.getByRole("link", { name: "Sign in with Contoso Entra ID" });
  await expect(entra).toBeVisible();
  await expect(sso.getByRole("link", { name: "Sign in with Keycloak" })).toBeVisible();
  // A navigation, not a fetch: the API redirects to the provider and back to where the operator was going.
  await expect(entra).toHaveAttribute("href", `/api/v1/auth/oidc/${unknown}/start?returnTo=${encodeURIComponent("/cis?q=crm")}`);
  await expect(page.getByTestId("directory-hint")).toContainText("directory account");
  await snap(page, "86-login-sso");

  // Following the link goes through the real API: an unknown provider lands back here with ssoError=unavailable.
  await entra.click();
  await expect(page).toHaveURL(/\/login\?ssoError=unavailable$/);
  await expect(page.getByTestId("sso-error")).toContainText(SSO_CODES.unavailable);
  await page.context().close();
});

test("sign-in page: every ssoError code has its own message; an unknown one still says what happened", async ({ browser }) => {
  const page = await anonymousPage(browser);
  await mockProviders(page, { oidc: [], directory: false });
  for (const [code, message] of Object.entries(SSO_CODES)) {
    await page.goto(`/login?ssoError=${code}`);
    const alert = page.getByTestId("sso-error");
    await expect(alert).toContainText("Single sign-on did not work.");
    await expect(alert).toContainText(message);
  }
  await page.goto("/login?ssoError=something_new");
  await expect(page.getByTestId("sso-error")).toContainText("(something_new)");
  // Without providers there are no buttons and no directory hint; the password form is unchanged.
  await expect(page.getByRole("navigation", { name: "Single sign-on" })).toHaveCount(0);
  await expect(page.getByTestId("directory-hint")).toHaveCount(0);
  await expect(page.getByLabel("Username")).toBeVisible();
  await page.context().close();
});

test("sign-in page: an unreachable directory (503 IDENTITY_PROVIDER_UNAVAILABLE) is explained, local accounts still named", async ({ browser }) => {
  const page = await anonymousPage(browser);
  await mockProviders(page, { oidc: [], directory: true });
  await page.route("**/api/v1/auth/login", (route) =>
    route.fulfill({
      status: 503,
      json: { error: { code: "IDENTITY_PROVIDER_UNAVAILABLE", message: "The directory could not be reached", details: [], requestId: "e2e" } },
    }),
  );
  await page.goto("/login?ssoError=failed");
  await page.getByLabel("Username").fill("jdoe");
  await page.getByLabel("Password").fill("directory-password");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("could not be reached, so directory accounts cannot sign in right now");
  await expect(page.getByRole("alert")).toContainText("Local accounts still work");
  // Trying again with a password clears the previous single sign-on error.
  await expect(page.getByTestId("sso-error")).toHaveCount(0);
  await expect(page).toHaveURL(/\/login$/);
  await page.context().close();
});

// ---------- Administration › Identity providers ----------

test("create an OIDC provider: required fields, the API's field errors, then a write-only secret and a group mapping", async ({ page, request }) => {
  await page.goto("/admin");
  await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Identity providers" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Identity providers" })).toBeVisible();
  await page.getByRole("link", { name: "+ New OpenID Connect provider" }).first().click();
  await expect(page.getByRole("heading", { level: 1, name: "New identity provider" })).toBeVisible();
  await expect(page.getByRole("radio", { name: /^OpenID Connect/ })).toBeChecked();
  // The server's defaults are filled in.
  await expect(page.getByLabel("Username claim")).toHaveValue("preferred_username");
  await expect(page.getByLabel("Groups claim")).toHaveValue("groups");
  // A new provider verifies MFA from the sign-in token by default.
  await expect(page.getByRole("radio", { name: /^Verify from the sign-in token/ })).toBeChecked();
  await expect(page.getByLabel("Required ACR values")).toHaveValue("");
  await expect(page.getByTestId("mfa-trust-warning")).toHaveCount(0);

  // Nothing filled: the form says what is missing without a round trip.
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page.locator("#idp-name-err")).toHaveText("Required");
  await expect(page.locator("#idp-oidc-issuerUrl-err")).toHaveText("Required");
  await expect(page.locator("#idp-name")).toBeFocused();

  // Plain http to another host is refused by the API; the message lands next to the issuer URL.
  await page.locator("#idp-name").fill(OIDC_NAME);
  await page.getByLabel("Issuer URL").fill("http://idp.example.com/realms/e2e");
  await page.getByLabel("Client ID").fill("shadoucmdb-e2e");
  await page.getByLabel("Client secret").fill(SECRET);
  await page.getByRole("button", { name: "+ Add mapping" }).click();
  await page.getByLabel("Group of mapping 1").fill("CMDB-Admins");
  await page.getByLabel("Permission profile of mapping 1").selectOption({ label: "Administrator" });
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page.locator("#idp-oidc-issuerUrl-err")).toContainText("https");
  await expect(page.getByLabel("Issuer URL")).toHaveAttribute("aria-invalid", "true");
  await expect(page.getByRole("alert").first()).toContainText("Not saved — fix the highlighted fields.");

  await page.getByLabel("Issuer URL").fill("https://127.0.0.1:9/realms/e2e");
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page).toHaveURL(/\/admin\/identity-providers\/[0-9a-f-]{36}$/);
  oidcId = page.url().split("/").pop()!;
  await expect(page.getByRole("heading", { level: 1, name: OIDC_NAME })).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: `Created ${OIDC_NAME}` })).toBeVisible();

  // The secret is stored, never shown; the page holds no copy of it.
  await expect(page.locator("#idp-oidc-clientSecret-state")).toHaveText("Stored — never shown");
  expect(await page.content()).not.toContain(SECRET);
  const saved = await provider(request, oidcId);
  expect(saved.oidc?.clientSecretSet).toBe(true);
  expect(saved.oidc).toMatchObject({ mfaAssurance: "verify", requiredAcr: [] });
  expect(JSON.stringify(saved)).not.toContain(SECRET);
  expect(saved.groupMappings).toEqual([expect.objectContaining({ group: "CMDB-Admins", profileName: "Administrator" })]);

  // The redirect URI to register at the provider, or why there is none yet.
  if (saved.oidc?.redirectUri) {
    await expect(page.getByLabel("Redirect URI")).toHaveValue(saved.oidc.redirectUri);
  } else {
    await expect(page.getByTestId("public-url-missing")).toContainText("PUBLIC_URL");
  }
  await snap(page, "86-oidc-provider");
});

test("secrets: left alone they are kept, Replace sets a new one, Remove deletes it", async ({ page, request }) => {
  await page.goto(`/admin/identity-providers/${oidcId}`);
  await expect(page.getByRole("heading", { level: 1, name: OIDC_NAME })).toBeVisible();

  // A change elsewhere keeps the secret.
  await page.getByLabel("Scopes").fill("profile email groups");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${OIDC_NAME}` })).toBeVisible();
  let saved = await provider(request, oidcId);
  expect(saved.oidc?.clientSecretSet).toBe(true);

  // Replace: the PATCH carries the new secret (and only then).
  await page.getByRole("button", { name: "Replace…" }).click();
  await expect(page.getByLabel("Client secret")).toBeFocused();
  await page.getByLabel("Client secret").fill(SECRET_2);
  const patch = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${oidcId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  expect((await patch).postDataJSON().oidc.clientSecret).toBe(SECRET_2);
  await expect(page.locator("#idp-oidc-clientSecret-state")).toHaveText("Stored — never shown");
  expect(await page.content()).not.toContain(SECRET_2);

  // Remove, undo, remove again: null is sent, and it is gone.
  await page.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(page.locator("#idp-oidc-clientSecret-state")).toHaveText("Will be removed when you save");
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(page.locator("#idp-oidc-clientSecret-state")).toHaveText("Stored — never shown");
  await page.getByRole("button", { name: "Remove", exact: true }).click();
  const removal = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${oidcId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  expect((await removal).postDataJSON().oidc.clientSecret).toBeNull();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${OIDC_NAME}` })).toBeVisible();
  saved = await provider(request, oidcId);
  expect(saved.oidc?.clientSecretSet).toBe(false);
  await expect(page.getByLabel("Client secret")).toHaveValue("");
});

test("MFA setting: required ACR values round-trip under Verify; Trust warns, drops them and badges the provider", async ({ page, request }) => {
  await page.goto(`/admin/identity-providers/${oidcId}`);
  await expect(page.getByRole("heading", { level: 1, name: OIDC_NAME })).toBeVisible();
  const verify = page.getByRole("radio", { name: /^Verify from the sign-in token/ });
  const trust = page.getByRole("radio", { name: /^Trust the provider without checking/ });
  const acr = page.getByLabel("Required ACR values");
  await expect(verify).toBeChecked();

  // Checked before any request: at most 10 values, printable ASCII.
  await acr.fill(Array.from({ length: 11 }, (_, i) => `loa${i}`).join(" "));
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator("#idp-oidc-requiredAcr-err")).toHaveText("At most 10 values");
  await expect(acr).toBeFocused();
  await acr.fill("urn:x:gold\u00e9");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator("#idp-oidc-requiredAcr-err")).toContainText("printable ASCII");

  // Separated by spaces or line breaks, sent as a list, each value once.
  await acr.fill("  urn:x:gold   loa3 urn:x:gold ");
  let patch = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${oidcId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  expect((await patch).postDataJSON().oidc).toMatchObject({ mfaAssurance: "verify", requiredAcr: ["urn:x:gold", "loa3"] });
  await expect(page.getByRole("status").filter({ hasText: `Saved ${OIDC_NAME}` })).toBeVisible();
  expect((await provider(request, oidcId)).oidc).toMatchObject({ mfaAssurance: "verify", requiredAcr: ["urn:x:gold", "loa3"] });
  await page.reload();
  await expect(acr).toHaveValue("urn:x:gold loa3");

  // Trust: the list goes away, a warning says what it means, and the API empties the list.
  await trust.check();
  await expect(acr).toHaveCount(0);
  const warning = page.getByTestId("mfa-trust-warning");
  await expect(warning).toContainText("will not check that the provider used a second factor");
  await expect(warning).toContainText("depend entirely on the identity provider's policy");
  patch = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${oidcId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  const sent = (await patch).postDataJSON().oidc;
  expect(sent.mfaAssurance).toBe("trustProvider");
  expect(sent).not.toHaveProperty("requiredAcr");
  await expect(page.getByRole("status").filter({ hasText: `Saved ${OIDC_NAME}` })).toBeVisible();
  expect((await provider(request, oidcId)).oidc).toMatchObject({ mfaAssurance: "trustProvider", requiredAcr: [] });
  await expect(page.locator(".page-header .badge.warn")).toHaveText("MFA not verified");
  await page.reload();
  await expect(trust).toBeChecked();
  await snap(page, "131-oidc-mfa-trust");

  // The list badges it, visibly: next to the "No group mappings" warning the Name cell wraps
  // instead of clipping the badge out of view (toHaveText/toBeVisible ignore overflow clipping).
  await page.route("**/api/v1/admin/identity-providers", async (route: Route) => {
    const body = (await (await route.fetch()).json()) as Provider[];
    await route.fulfill({ json: body.map((p) => (p.id === oidcId ? { ...p, groupMappings: [] } : p)) });
  });
  await page.goto("/admin/identity-providers");
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: OIDC_NAME, exact: true }) });
  await expect(row.getByText("No group mappings: nobody can sign in")).toBeVisible();
  const badge = row.getByTestId("mfa-not-verified");
  await expect(badge).toHaveText("MFA not verified");
  const cell = (await row.locator("td").nth(1).boundingBox())!;
  const box = (await badge.boundingBox())!;
  expect(box.x + box.width).toBeLessThanOrEqual(cell.x + cell.width + 0.5);
  expect(box.y + box.height).toBeLessThanOrEqual(cell.y + cell.height + 0.5);
  await snap(page, "131-oidc-mfa-list-badge");
  await page.unroute("**/api/v1/admin/identity-providers");

  // Back to Verify: the badge goes, the list starts empty (amr decides).
  await page.goto(`/admin/identity-providers/${oidcId}`);
  await verify.check();
  await expect(acr).toHaveValue("");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${OIDC_NAME}` })).toBeVisible();
  expect((await provider(request, oidcId)).oidc).toMatchObject({ mfaAssurance: "verify", requiredAcr: [] });
  await expect(page.locator(".page-header .badge.warn")).toHaveCount(0);
  await page.goto("/admin/identity-providers");
  await expect(page.getByRole("row").filter({ has: page.getByRole("link", { name: OIDC_NAME, exact: true }) }).getByTestId("mfa-not-verified")).toHaveCount(0);
});

test("the connection test reports what failed, and warns when there are unsaved changes", async ({ page }) => {
  await page.goto(`/admin/identity-providers/${oidcId}`);
  const panel = page.getByRole("region", { name: "Test connection" });
  await page.getByLabel("Client ID").fill("changed-but-not-saved");
  await expect(panel.getByRole("status").filter({ hasText: "Save your changes first" })).toBeVisible();
  await page.getByLabel("Client ID").fill("shadoucmdb-e2e");
  await expect(panel.getByText("Save your changes first")).toHaveCount(0);
  await panel.getByRole("button", { name: "Run test" }).click();
  const result = panel.getByTestId("test-result");
  await expect(result).toContainText("Test failed", { timeout: 30_000 });
  await expect(result).not.toBeEmpty();
});

test("the sign-in page offers the new provider (when PUBLIC_URL is set)", async ({ browser, request }) => {
  const saved = await provider(request, oidcId);
  test.skip(!saved.oidc?.redirectUri, "PUBLIC_URL is not set on this server, so the sign-in page lists no OIDC provider");
  const page = await anonymousPage(browser);
  await page.goto("/login");
  const link = page.getByRole("navigation", { name: "Single sign-on" }).getByRole("link", { name: `Sign in with ${OIDC_NAME}` });
  await expect(link).toHaveAttribute("href", `/api/v1/auth/oidc/${oidcId}/start`);
  await page.context().close();
});

test("create an LDAP directory: StartTLS follows the URL, a bind DN needs its password, the lookup test runs", async ({ page, request }) => {
  await page.goto("/admin/identity-providers");
  await page.getByRole("link", { name: "+ New LDAP directory" }).first().click();
  await expect(page.getByRole("radio", { name: /^LDAP \/ Active Directory/ })).toBeChecked();
  await expect(page.getByLabel("User filter")).toHaveValue("(&(objectClass=user)(sAMAccountName={username}))");
  // Directory accounts set up MFA in ShadouCMDB: no provider MFA setting.
  await expect(page.getByRole("group", { name: "Multi-factor authentication" })).toHaveCount(0);
  await page.locator("#idp-name").fill(LDAP_NAME);
  // Disabled, so that no other spec's password sign-in asks this (unreachable) directory.
  await page.getByLabel("Enabled (users can sign in through it)").uncheck();
  await page.getByLabel("Server URL").fill("ldap://127.0.0.1:9");
  await expect(page.locator("#idp-ldap-url-hint")).toContainText("StartTLS");
  await page.getByLabel("Server URL").fill("ldaps://127.0.0.1:9");
  await expect(page.locator("#idp-ldap-url-hint")).toContainText("LDAPS");
  await page.getByLabel("Service account (bind DN)").fill("CN=svc-cmdb,OU=Service Accounts,DC=example,DC=com");
  await page.getByLabel("User search base").fill("OU=Staff,DC=example,DC=com");
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page.locator("#idp-ldap-bindPassword-err")).toHaveText("A bind DN needs its password");
  await expect(page.getByRole("status").filter({ hasText: "No mappings yet" })).toBeVisible();

  await page.getByLabel("Service account password").fill(BIND_PASSWORD);
  await page.getByRole("button", { name: "+ Add mapping" }).click();
  await page.getByLabel("Group of mapping 1").fill("CN=CMDB Operators,OU=Groups,DC=example,DC=com");
  await page.getByLabel("Permission profile of mapping 1").selectOption({ label: "Administrator" });
  await page.getByRole("button", { name: "+ Add mapping" }).click();
  await page.getByLabel("Group of mapping 2").fill("CN=CMDB Readers,OU=Groups,DC=example,DC=com");
  // Left without a profile: said next to the row before any request.
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page.getByText("Choose a profile", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Remove mapping 2" }).click();
  await page.getByRole("button", { name: "Create provider" }).click();
  await expect(page).toHaveURL(/\/admin\/identity-providers\/[0-9a-f-]{36}$/);
  ldapId = page.url().split("/").pop()!;

  const saved = await provider(request, ldapId);
  expect(saved.isEnabled).toBe(false);
  expect(saved.ldap).toMatchObject({ bindPasswordSet: true, startTls: false });
  expect(saved.groupMappings).toHaveLength(1);
  expect(await page.content()).not.toContain(BIND_PASSWORD);
  await expect(page.locator("#idp-ldap-bindPassword-state")).toHaveText("Stored — never shown");

  const panel = page.getByRole("region", { name: "Test connection" });
  await panel.getByLabel("Look up a user (optional)").fill("jdoe");
  await panel.getByRole("button", { name: "Run test" }).click();
  await expect(panel.getByTestId("test-result")).toContainText("Test failed", { timeout: 30_000 });
  await snap(page, "86-ldap-provider");

  // Listed with its type, status and mapping count.
  await page.goto("/admin/identity-providers");
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: LDAP_NAME, exact: true }) });
  await expect(row).toContainText("LDAP / Active Directory");
  await expect(row).toContainText("Disabled");
  await expect(row.getByRole("cell", { name: "ldaps://127.0.0.1:9" })).toBeVisible();
});

test("a changed server address asks for the bind password again; the API's secret_required opens the input too", async ({ page, request }) => {
  await page.goto(`/admin/identity-providers/${ldapId}`);
  const state = page.locator("#idp-ldap-bindPassword-state");
  const url = page.getByLabel("Server URL");
  const bindDn = page.getByLabel("Service account (bind DN)");
  const password = page.getByLabel("Service account password");
  const hint = page.locator("#idp-ldap-bindPassword-hint");
  const reentry = "The server address changed. Enter the bind password again.";
  await expect(state).toHaveText("Stored — never shown");

  // Another port: the stored password would go to a different server, so it has to be typed again.
  await url.fill("ldaps://127.0.0.1:10");
  await expect(password).toHaveValue("");
  await expect(password).toHaveAttribute("aria-required", "true");
  await expect(hint).toHaveText(reentry);
  await expect(page.getByRole("button", { name: "Keep stored" })).toHaveCount(0);
  // The stored address typed back (written differently) keeps the stored password.
  await url.fill("ldaps://127.0.0.1:9/");
  await expect(state).toHaveText("Stored — never shown");
  await expect(hint).not.toHaveText(reentry);
  // So does the bind DN.
  await bindDn.fill("CN=someone-else,DC=example,DC=com");
  await expect(hint).toHaveText(reentry);
  await bindDn.fill("CN=svc-cmdb,OU=Service Accounts,DC=example,DC=com");
  await expect(state).toHaveText("Stored — never shown");

  // Saving the new address without the password is refused before any request.
  await url.fill("ldaps://127.0.0.1:10");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator("#idp-ldap-bindPassword-err")).toHaveText("Required");
  await expect(password).toBeFocused();
  await password.fill(BIND_PASSWORD_2);
  let patch = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${ldapId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  expect((await patch).postDataJSON().ldap).toMatchObject({ url: "ldaps://127.0.0.1:10", bindPassword: BIND_PASSWORD_2 });
  await expect(page.getByRole("status").filter({ hasText: `Saved ${LDAP_NAME}` })).toBeVisible();
  await expect(state).toHaveText("Stored — never shown");
  expect(await page.content()).not.toContain(BIND_PASSWORD_2);

  // Meanwhile another administrator moves the directory. This page still holds the old address, so
  // its next save would move it back without the password: the API refuses with secret_required, and
  // the form opens the password input with the server's message instead of a generic error.
  await apiSend(request, "PATCH", `/admin/identity-providers/${ldapId}`, { ldap: { url: "ldaps://127.0.0.1:11", bindPassword: BIND_PASSWORD } });
  await page.getByLabel("User search base").fill("OU=People,DC=example,DC=com");
  patch = page.waitForRequest((r) => r.method() === "PATCH" && r.url().endsWith(`/identity-providers/${ldapId}`));
  await page.getByRole("button", { name: "Save changes" }).click();
  expect((await patch).postDataJSON().ldap).not.toHaveProperty("bindPassword");
  await expect(page.getByRole("alert").first()).toContainText("Not saved");
  await expect(page.locator("#idp-ldap-bindPassword-err")).toContainText("again");
  await expect(password).toBeFocused();
  await expect(hint).toHaveText(reentry);
  await password.fill(BIND_PASSWORD);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${LDAP_NAME}` })).toBeVisible();
  const saved = await provider(request, ldapId);
  expect(saved.ldap?.bindPasswordSet).toBe(true);
  await expect(state).toHaveText("Stored — never shown");
});

test("deleting a provider that still has accounts (409 IN_USE) offers to disable it instead", async ({ page, request }) => {
  await page.goto(`/admin/identity-providers/${oidcId}`);
  await expect(page.getByRole("heading", { level: 1, name: OIDC_NAME })).toBeVisible();
  // No real sign-in can create an account here: answer the delete as the server does for a provider in use.
  await page.route(`**/api/v1/admin/identity-providers/${oidcId}`, (route) =>
    route.request().method() === "DELETE"
      ? route.fulfill({
          status: 409,
          json: { error: { code: "IN_USE", message: "3 accounts sign in through this provider", details: [], requestId: "e2e" } },
        })
      : route.fallback(),
  );
  await page.getByRole("button", { name: "Delete provider" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete identity provider ${OIDC_NAME}?` });
  await expect(dialog).toContainText("group mapping");
  await dialog.getByRole("button", { name: "Delete provider" }).click();
  await expect(dialog.getByRole("alert")).toContainText(`${OIDC_NAME} still has accounts.`);
  await expect(dialog.getByRole("alert")).toContainText("3 accounts sign in through this provider");
  await snap(page, "86-delete-in-use");
  await dialog.getByRole("button", { name: "Disable instead" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status").filter({ hasText: `${OIDC_NAME} is disabled` })).toBeVisible();
  await expect(page.locator(".page-header .badge.off")).toHaveText("Disabled");
  expect((await provider(request, oidcId)).isEnabled).toBe(false);

  // Enable again through the confirmation.
  await page.getByRole("button", { name: "Enable provider" }).click();
  await page.getByRole("dialog", { name: `Enable ${OIDC_NAME}?` }).getByRole("button", { name: "Enable provider" }).click();
  await expect(page.getByRole("status").filter({ hasText: `${OIDC_NAME} is enabled.` })).toBeVisible();
  expect((await provider(request, oidcId)).isEnabled).toBe(true);
});

test("a provider without accounts is deleted, and is gone from the list", async ({ page, request }) => {
  await page.goto(`/admin/identity-providers/${ldapId}`);
  await page.getByRole("button", { name: "Delete provider" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete identity provider ${LDAP_NAME}?` });
  await dialog.getByRole("button", { name: "Delete provider" }).click();
  await expect(page).toHaveURL(/\/admin\/identity-providers$/);
  await expect(page.getByRole("link", { name: LDAP_NAME, exact: true })).toHaveCount(0);
  const res = await request.get(`/api/v1/admin/identity-providers/${ldapId}`);
  expect(res.status()).toBe(404);
  ldapId = "";
});

test("users.manage alone is not enough: no section, a permission-denied screen, and the API refuses reads and changes", async ({ browser, playwright, baseURL }) => {
  const page = await anonymousPage(browser);
  await page.goto("/login");
  await page.getByLabel("Username").fill(MANAGER);
  await page.getByLabel("Password").fill(MANAGER_PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await page.goto("/admin/users");
  const nav = page.getByRole("navigation", { name: "Administration" });
  await expect(nav.getByRole("link", { name: "Users" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Identity providers" })).toHaveCount(0);
  await page.goto("/admin/identity-providers");
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();
  await expect(page.getByText("only for holders of the built-in")).toBeVisible();
  await page.context().close();

  // The server is what refuses it: a user manager cannot read a provider's mappings, nor add or change a provider
  // (and so cannot grant themselves the Administrator profile through a group mapping).
  const ctx = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const login = await ctx.post("/api/v1/auth/login", { data: { username: MANAGER, password: MANAGER_PASSWORD } });
  expect(login.status()).toBe(200);
  const headers = { "X-CSRF-Token": (await login.json()).csrfToken as string };
  const list = await ctx.get("/api/v1/admin/identity-providers");
  expect(list.status(), await list.text()).toBe(403);
  const read = await ctx.get(`/api/v1/admin/identity-providers/${oidcId}`);
  expect(read.status(), await read.text()).toBe(403);
  const create = await ctx.post("/api/v1/admin/identity-providers", {
    headers,
    data: { kind: "oidc", name: `E2E refused ${stamp}`, oidc: { issuerUrl: "https://127.0.0.1:9/", clientId: "x" } },
  });
  expect(create.status(), await create.text()).toBe(403);
  const change = await ctx.patch(`/api/v1/admin/identity-providers/${oidcId}`, { headers, data: { isEnabled: false } });
  expect(change.status(), await change.text()).toBe(403);
  await ctx.dispose();
});

test("users: a provider's account shows where it signs in, has no password reset, and warns that edits are overwritten", async ({ page }) => {
  // Accounts are only created by a real sign-in through the provider; show the manager account as one.
  await page.route(`**/api/v1/admin/users/${managerId}`, async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    const user = await res.json();
    await route.fulfill({ response: res, json: { ...user, identityProvider: { id: oidcId, name: OIDC_NAME, kind: "oidc" } } });
  });
  await page.route("**/api/v1/admin/users?*", async (route) => {
    const res = await route.fetch();
    const list = await res.json();
    for (const u of list.data) if (u.id === managerId) u.identityProvider = { id: oidcId, name: OIDC_NAME, kind: "oidc" };
    await route.fulfill({ response: res, json: list });
  });

  await page.goto(`/admin/users?q=${MANAGER}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: MANAGER, exact: true }) });
  await expect(row).toContainText(OIDC_NAME);
  await row.getByRole("link", { name: MANAGER, exact: true }).click();

  await expect(page.getByTestId("user-provider")).toHaveText(`Signs in with ${OIDC_NAME}`);
  const notice = page.getByTestId("provider-notice");
  await expect(notice).toContainText("overwritten");
  await expect(notice.getByRole("link", { name: OIDC_NAME })).toHaveAttribute("href", `/admin/identity-providers/${oidcId}`);
  await expect(page.getByRole("heading", { name: "Reset password" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Set new password" })).toHaveCount(0);
  await expect(page.getByTestId("provider-credentials")).toContainText("has no password in ShadouCMDB");
  await snap(page, "86-provider-user");
});

test("local accounts keep the password reset", async ({ page }) => {
  await page.goto(`/admin/users/${managerId}`);
  await expect(page.getByRole("heading", { name: "Reset password" })).toBeVisible();
  await expect(page.getByTestId("provider-notice")).toHaveCount(0);
});
