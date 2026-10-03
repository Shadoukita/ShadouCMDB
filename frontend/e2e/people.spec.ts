import type { Page as BrowserPage } from "@playwright/test";
import { apiGet, apiSend, at, checkA11y, expect, snap, test } from "./support";

// Users ↔ Person CIs (SHAA-1505, UI in SHAA-1509): every sign-in account is linked 1 : 1 to a Person CI through its
// e-mail. An administrator creates a user (required e-mail, conflicts worded next to the field), follows the link to
// the Person, sees its Sign-in account panel and its read-only Email, cannot delete it while linked, and filters the
// Users page by sign-in state. An account from before e-mails were required is stopped at an e-mail step.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = `people-e2e-password-${stamp}`;
const USERNAME = `e2e-person-${stamp}`;
const DISPLAY = `E2E Person ${stamp}`;
const EMAIL = `${USERNAME}@example.test`;

interface Page_<T> {
  data: T[];
}
interface User {
  id: string;
  username: string;
  email: string | null;
  person: { id: string; label: string } | null;
  signInStatus: "ready" | "email_required" | "person_missing";
}
interface Ci {
  id: string;
  label: string;
  attributes: Record<string, unknown>;
}

async function personClassId(request: Parameters<typeof apiGet>[0]): Promise<string> {
  const classes = await apiGet<Page_<{ id: string; systemRole: string | null }>>(request, "/ci-classes?limit=200");
  const person = classes.data.find((c) => c.systemRole === "person");
  expect(person, "the built-in Person class").toBeTruthy();
  return person!.id;
}

let userId = "";
let personId = "";

test("an administrator creates a user: the e-mail is required, unique, and links a Person", async ({ page, request }) => {
  const other = await apiSend<User>(request, "POST", "/admin/users", {
    username: `e2e-person-other-${stamp}`,
    displayName: `E2E Other ${stamp}`,
    email: `e2e-person-other-${stamp}@example.test`,
    password: PASSWORD,
    profileIds: [],
  });

  await page.goto("/admin/users/new");
  await expect(page.getByRole("textbox", { name: "Email" })).toHaveAttribute("aria-required", "true");
  await page.locator("#user-username").fill(USERNAME);
  await page.locator("#user-displayName").fill(DISPLAY);
  await page.locator("#user-password").fill(PASSWORD);
  await page.locator("#user-confirm").fill(PASSWORD);
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.locator("#user-email-err")).toHaveText("Required");

  // Another account's address, in another case: refused next to the field, in our words.
  await page.locator("#user-email").fill(other.email!.toUpperCase());
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.locator("#user-email-err")).toHaveText("Another account already uses this e-mail address.");
  await expect(page.locator("#user-email")).toHaveAttribute("aria-invalid", "true");

  await page.locator("#user-email").fill(EMAIL);
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.getByRole("status").first()).toContainText(`Created user ${USERNAME}.`);
  userId = page.url().split("/").pop()!;

  // The Person was created for it (Name = display name) and the user page links to it.
  const user = await apiGet<User>(request, `/admin/users/${userId}`);
  expect(user.signInStatus).toBe("ready");
  expect(user.person?.label).toBe(DISPLAY);
  personId = user.person!.id;
  await expect(page.getByTestId("user-person").getByRole("link", { name: DISPLAY })).toHaveAttribute("href", `/cis/${personId}`);
  await expect(page.getByTestId("user-sign-in-status")).toHaveCount(0);
  await snap(page, "people-01-user-created");
});

test("the Person shows its sign-in account, a read-only Email, and cannot be deleted", async ({ page }, testInfo) => {
  await page.goto(`/admin/users/${userId}`);
  await page.getByTestId("user-person").getByRole("link", { name: DISPLAY }).click();
  await expect(page).toHaveURL(at(`/cis/${personId}`));
  await expect(page.getByRole("heading", { level: 1, name: DISPLAY })).toBeVisible();

  const panel = page.getByRole("region", { name: "Sign-in account" });
  await expect(panel.getByRole("link", { name: USERNAME })).toHaveAttribute("href", `/admin/users/${userId}`);
  await expect(panel).toContainText(DISPLAY);
  await expect(panel).toContainText("Active");
  await snap(page, "people-02-person-panel");
  await checkA11y(page, testInfo, "person-detail", { include: "[data-testid=sign-in-account]", strict: true });

  // Deleting is refused up front: the dialog names the account and keeps its confirm button disabled.
  await page.getByRole("button", { name: "Delete" }).click();
  const confirm = page.getByRole("dialog");
  await expect(confirm.getByTestId("person-linked")).toContainText(`linked to the sign-in account ${USERNAME}`);
  await expect(confirm.getByRole("button", { name: /^Delete CI/ })).toBeDisabled();
  await confirm.getByRole("button", { name: "Cancel" }).click();

  // The Email follows the account: read-only on the form, saying who manages it. Other fields stay editable.
  await page.getByRole("link", { name: "Edit" }).click();
  const email = page.locator("#attr-email");
  await expect(email).toHaveValue(EMAIL);
  await expect(email).toBeDisabled();
  await expect(page.locator("#attr-email-hint")).toHaveText(`Managed by user ${USERNAME}`);
  await expect(page.locator("#attr-name")).toBeEnabled();
  await snap(page, "people-03-person-email-read-only");
});

test("changing the user's e-mail moves the Person's; another person's address is refused", async ({ page, request }) => {
  const taken = `e2e-person-taken-${stamp}@example.test`;
  await apiSend(request, "POST", "/configuration-items", { classId: await personClassId(request), attributes: { name: `E2E Contact ${stamp}`, email: taken } });

  await page.goto(`/admin/users/${userId}`);
  await page.locator("#user-email").fill(taken);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator("#user-email-err")).toContainText("A person who is not this account's already has this e-mail address.");

  const moved = `e2e-person-moved-${stamp}@example.test`;
  await page.locator("#user-email").fill(moved);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByText(`Saved ${USERNAME}.`)).toBeVisible();
  expect((await apiGet<Ci>(request, `/configuration-items/${personId}`)).attributes.email).toBe(moved);

  await page.goto(`/cis/${personId}/edit`);
  await expect(page.locator("#attr-email")).toHaveValue(moved);
});

test("the Users page links each account's Person and filters by sign-in state in the URL", async ({ page }, testInfo) => {
  await page.goto(`/admin/users?q=${USERNAME}`);
  const row = page.locator("tbody tr", { has: page.getByRole("link", { name: USERNAME, exact: true }) });
  await expect(row.getByRole("link", { name: DISPLAY })).toHaveAttribute("href", `/cis/${personId}`);
  await checkA11y(page, testInfo, "users-person");

  await page.getByLabel("Sign-in state").selectOption("person_missing");
  await expect(page).toHaveURL(/signInStatus=person_missing/);
  await expect(page.getByText("No users match these filters")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Sign-in state")).toHaveValue("person_missing");

  await page.getByLabel("Sign-in state").selectOption("ready");
  await expect(row).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page).not.toHaveURL(/signInStatus=/);
  await snap(page, "people-04-users-page");
});

test("the data model keeps the Person's Name and Email", async ({ page, request }) => {
  await page.goto(`/admin/classes/${await personClassId(request)}`);
  for (const label of ["Name", "Email"]) {
    const row = page.locator("tbody tr", { has: page.getByRole("button", { name: label, exact: true }) });
    await expect(row.getByTestId("system-attribute")).toBeVisible();
    await expect(row.getByRole("button", { name: `Archive ${label}` })).toBeDisabled();
  }
  await page.getByRole("button", { name: "Email", exact: true }).click();
  await expect(page.locator("#ad-required")).toBeDisabled();
  await expect(page.locator("#ad-type")).toBeDisabled();
});

/**
 * An account from before e-mails were required. The API cannot make one any more (the e-mail is required), so the
 * session is answered as the API answers such an account: `emailRequired` until PUT /auth/email succeeds.
 */
async function asAccountWithoutEmail(page: BrowserPage, onEmail: (email: string) => { status: number; body?: unknown } | null) {
  let required = true;
  await page.route("**/api/v1/auth/me", async (route) => {
    const res = await route.fetch();
    const session = await res.json();
    await route.fulfill({ response: res, json: { ...session, emailRequired: required, user: { ...session.user, email: required ? null : session.user.email } } });
  });
  await page.route("**/api/v1/auth/email", async (route) => {
    const email = (route.request().postDataJSON() as { email: string }).email;
    const refusal = onEmail(email);
    if (refusal) return route.fulfill({ status: refusal.status, json: refusal.body });
    required = false;
    const me = await page.request.get("/api/v1/auth/me");
    await route.fulfill({ status: 200, json: { ...(await me.json()), emailRequired: false } });
  });
}

test("an account without an e-mail enters one before anything else, then goes where it was going", async ({ page }, testInfo) => {
  const sent: string[] = [];
  await asAccountWithoutEmail(page, (email) => {
    sent.push(email);
    if (sent.length > 1) return null;
    const message = "Another account already uses this e-mail address";
    return { status: 409, body: { error: { code: "CONFLICT", message, details: [{ in: "body", field: "email", message, code: "unique" }] } } };
  });

  await page.goto("/cis?q=router");
  // The router leaves "/" and "?" unencoded in the query, so compare the decoded return path.
  await expect(page).toHaveURL((url) => url.pathname === "/enter-email" && url.searchParams.get("redirect") === "/cis?q=router");
  const form = page.getByTestId("email-entry");
  await expect(form.getByRole("heading", { name: "Enter your e-mail address" })).toBeVisible();
  // It stands alone: no navigation, no search.
  await expect(page.getByRole("navigation", { name: "Main" })).toHaveCount(0);
  await checkA11y(page, testInfo, "email-entry", { include: "[data-testid=email-entry]", strict: true });
  await snap(page, "people-05-email-entry");

  await form.getByRole("button", { name: "Save and continue" }).click();
  await expect(page.locator("#email-entry-err")).toHaveText("Enter your e-mail address.");
  await form.getByRole("textbox", { name: "E-mail address" }).fill("not-an-address");
  await form.getByRole("button", { name: "Save and continue" }).click();
  await expect(page.locator("#email-entry-err")).toHaveText("Enter a valid e-mail address, such as name@example.com.");
  expect(sent).toEqual([]);

  await form.getByRole("textbox", { name: "E-mail address" }).fill("taken@example.test");
  await form.getByRole("button", { name: "Save and continue" }).click();
  await expect(page.locator("#email-entry-err")).toHaveText("Another account already uses this e-mail address.");

  await form.getByRole("textbox", { name: "E-mail address" }).fill(" legacy@example.test ");
  await form.getByRole("button", { name: "Save and continue" }).click();
  await expect(page).toHaveURL(at("/cis", "?q=router"));
  expect(sent).toEqual(["taken@example.test", "legacy@example.test"]);
  // Done: the step is not reachable any more.
  await page.goto("/enter-email");
  await expect(page).toHaveURL(at("/"));
});

test("an SSO sign-in refused for an incomplete account says so", async ({ browser }) => {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login?ssoError=account_incomplete");
  await expect(page.getByTestId("sso-error")).toContainText("Your ShadouCMDB account is incomplete");
  await context.close();
});
