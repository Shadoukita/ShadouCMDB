// GH-192: over HTTPS the session and CSRF cookies carry the `__Host-` prefix, and the UI must
// echo the CSRF token of the session the API actually reads (the `__Host-` one), never a
// plain-named cookie that a sibling subdomain could have planted.
import { csrfFromCookies } from "../src/api/csrf";
import { E2E_USER } from "./global-setup";
import { expect, test } from "./support";

test("the UI reads the __Host- CSRF cookie first, whatever the order", () => {
  expect(csrfFromCookies("__Host-shadoucmdb_csrf=good; shadoucmdb_csrf=planted")).toBe("good");
  expect(csrfFromCookies("shadoucmdb_csrf=planted; __Host-shadoucmdb_csrf=good")).toBe("good");
  expect(csrfFromCookies("a=1; shadoucmdb_csrf=plain")).toBe("plain");
  // An empty __Host- cookie still shuts out the plain name (the session is read from __Host- too).
  expect(csrfFromCookies("shadoucmdb_csrf=planted; __Host-shadoucmdb_csrf=")).toBeUndefined();
  expect(csrfFromCookies("")).toBeUndefined();
  expect(csrfFromCookies("xshadoucmdb_csrf=other")).toBeUndefined();
});

test.describe("behind an HTTPS proxy", () => {
  test.use({ storageState: { cookies: [], origins: [] }, extraHTTPHeaders: { "X-Forwarded-Proto": "https" } });

  test("sign-in sets __Host- cookies, and a planted plain CSRF cookie does not break writes", async ({ page, context, baseURL }) => {
    await page.goto("/login");
    await page.getByLabel("Username").fill(E2E_USER.username);
    await page.getByLabel("Password").fill(E2E_USER.password);
    const login = page.waitForResponse((r) => r.url().endsWith("/api/v1/auth/login"));
    await page.getByRole("button", { name: "Sign in" }).click();
    const setCookies = (await (await login).headersArray()).filter((h) => h.name.toLowerCase() === "set-cookie").map((h) => h.value);
    const session = setCookies.find((c) => c.startsWith("__Host-shadoucmdb_session="))!;
    expect(session, "the session cookie carries the __Host- prefix").toBeTruthy();
    expect(session).toMatch(/; Path=\/;/);
    expect(session).toMatch(/; Secure/);
    expect(session).not.toMatch(/Domain=/i);
    await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();

    const names = (await context.cookies()).map((c) => c.name);
    expect(names).toContain("__Host-shadoucmdb_session");
    expect(names).toContain("__Host-shadoucmdb_csrf");

    // A cookie planted under the plain name must not become the CSRF header.
    await context.addCookies([{ name: "shadoucmdb_csrf", value: "0".repeat(64), url: baseURL! }]);
    const logout = page.waitForResponse((r) => r.url().endsWith("/api/v1/auth/logout"));
    await page.getByRole("button", { name: "Sign out" }).click();
    expect((await logout).status(), "sign-out (a CSRF-checked write) succeeds").toBe(204);
  });
});
