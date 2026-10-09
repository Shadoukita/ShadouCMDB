import { apiGet, apiSend, checkA11y, chooseTheme, csrf, expect, test } from "./support";

// Administration › Identity providers in the reference-mockup look (design document §0, step 12f-11): the inventory's
// head band above the list (breadcrumb, title with the count, the two New buttons, intro, order hint), mono teal names
// and state pills; on a provider's page the CI page's head band without tabs, with a tile, chips and the issuer in mono.
test.describe.configure({ mode: "serial" });

const STAMP = Date.now().toString(36);
const NAME = `E2E look IdP ${STAMP}`;
const ISSUER = "https://127.0.0.1:9/realms/e2e";
let id = "";

test.beforeAll(async ({ request }) => {
  const profiles = await apiGet<{ data: { id: string; name: string }[] }>(request, "/admin/profiles?limit=200");
  const reader = profiles.data.find((p) => p.name === "Reader") ?? profiles.data[0];
  const created = await apiSend<{ id: string }>(request, "POST", "/admin/identity-providers", {
    kind: "oidc",
    name: NAME,
    isEnabled: false,
    oidc: { issuerUrl: ISSUER, clientId: `e2e-look-${STAMP}`, clientSecret: "e2e-only-not-a-secret" },
    groupMappings: [{ group: "CMDB-Readers", profileId: reader.id }],
  });
  id = created.id;
});

test.afterAll(async ({ request }) => {
  if (id) await request.delete(`/api/v1/admin/identity-providers/${id}`, { headers: { "X-CSRF-Token": await csrf(request) } });
});

test("identity providers: head band, mono names, state pills", async ({ page }, testInfo) => {
  await page.goto("/admin/identity-providers");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Identity providers");
  await expect(head.getByRole("heading", { level: 1, name: "Identity providers" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.locator(".count")).toHaveClass(/\bmono\b/);
  await expect(head.getByRole("link", { name: "New OpenID Connect provider" })).toBeVisible();
  await expect(head.getByRole("link", { name: "New LDAP directory" })).toBeVisible();
  await expect(head.locator(".toolbar-hint")).toBeVisible();

  const list = page.getByRole("region", { name: "Identity providers" });
  await expect(list.locator("table.list-table th").nth(1)).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row").filter({ hasText: NAME });
  await expect(row.getByRole("link", { name: NAME })).toHaveClass(/\blist-name\b/);
  const pill = row.locator(".badge.off").filter({ hasText: "Disabled" });
  await expect(pill.locator(".status-dot")).toHaveCount(1);
  await expect(pill).toHaveCSS("border-radius", "999px");

  await checkA11y(page, testInfo, "admin-idp-list-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-idp-list-dark");
  await chooseTheme(page, "");
});

test("identity provider page: record head band with tile, chips and the issuer", async ({ page }, testInfo) => {
  await page.goto(`/admin/identity-providers/${id}`);
  const head = page.locator(".record-head.record-head-plain");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText(NAME);
  await expect(head.locator(".class-tile")).toBeVisible();
  await expect(head.getByRole("heading", { level: 1 })).toHaveText(NAME);
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge.off .status-dot")).toHaveCount(1);
  await expect(meta.locator(".badge").filter({ hasText: "OpenID Connect" })).toBeVisible();
  await expect(meta.locator(".badge").filter({ hasText: "0 accounts" })).toBeVisible();
  await expect(meta.locator(".record-meta-line .mono")).toHaveText(ISSUER);
  const mappings = page.locator("table.mappings");
  await expect(mappings).toHaveClass(/\blist-table\b/);
  await expect(mappings.locator("th").first()).toHaveCSS("text-transform", "none");

  await checkA11y(page, testInfo, "admin-idp-page-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "admin-idp-page-dark");
  await chooseTheme(page, "");
});

test("identity providers: the head texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/admin/identity-providers");
  const head = page.locator(".list-head");
  await expect(head.getByRole("heading", { level: 1 })).toHaveText("Identitätsanbieter");
  await expect(head.locator(".count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(head.getByRole("link", { name: "Neuer OpenID-Connect-Anbieter" })).toBeVisible();
  await expect(page.getByRole("row").filter({ hasText: NAME }).locator(".badge.off")).toHaveText("Deaktiviert");

  await page.goto(`/admin/identity-providers/${id}`);
  await expect(page.getByTestId("record-meta").locator(".badge").filter({ hasText: "0 Konten" })).toBeVisible();
});
