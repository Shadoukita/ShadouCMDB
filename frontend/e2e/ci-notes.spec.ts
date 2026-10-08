import type { APIRequestContext, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, classIdByName, createCi, expect, test } from "./support";

// The Notes tab on the CI page (gap G13, SHAA-2591) against the notes API (SHAA-2355): list, add, edit and delete,
// a version conflict, and the actions each user is offered. The API decides who may change a note (`canEdit`,
// `canDelete`); the UI only shows the actions it allows. The CIs are named "zz-…" so they never come first in a list
// another spec opens.

interface Note {
  id: string;
  body: string;
  version: number;
  canEdit: boolean;
  canDelete: boolean;
}

const stamp = Date.now().toString(36);

async function notesOf(request: APIRequestContext, ciId: string) {
  return (await apiGet<{ data: Note[]; page: { total: number } }>(request, `/configuration-items/${ciId}/notes?limit=50`)).data;
}

async function openNotes(page: Page, ciId: string) {
  await page.goto(`/cis/${ciId}`);
  await page.getByRole("tab", { name: /^Notes/ }).click();
  await expect(page.getByRole("region", { name: /^Notes/ })).toBeVisible();
}

test("an editor lists, adds, edits and deletes notes; a stale edit is caught", async ({ page, request }) => {
  const appId = await classIdByName(request, "Application");
  const ci = await createCi(request, appId, `zz-notes-${stamp}`);
  await openNotes(page, ci.id);
  const panel = page.getByRole("region", { name: /^Notes/ });

  // No notes yet: the empty state says what notes are for, the add box is there.
  await expect(page.getByRole("tab", { name: "Notes 0" })).toBeVisible();
  await expect(panel.getByRole("heading", { name: "No notes yet" })).toBeVisible();

  // A blank note is caught next to the field.
  const box = panel.getByLabel("Add a note");
  await box.fill("   ");
  await panel.getByRole("button", { name: "Add note" }).click();
  await expect(panel.getByText("Enter the note's text.")).toBeVisible();
  await expect(box).toHaveAttribute("aria-invalid", "true");

  // Line breaks are kept and the text is shown as typed (no Markdown, no HTML).
  await box.fill("Maintenance window: Sun 02:00-04:00\nCall **ops** <b>first</b>");
  await panel.getByRole("button", { name: "Add note" }).click();
  await expect(box).toHaveValue("");
  const stream = panel.getByTestId("note-stream");
  const first = stream.getByTestId("note").first();
  await expect(first.locator(".note-body")).toHaveText("Maintenance window: Sun 02:00-04:00\nCall **ops** <b>first</b>");
  await expect(first.locator("b")).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "Notes 1" })).toBeVisible();
  // The author, and the relative time with the absolute one on hover.
  const time = first.locator("time");
  await expect(time).toHaveAttribute("datetime", /^\d{4}-\d{2}-\d{2}T/);
  await expect(time).toHaveAttribute("title", /\d/);

  // Newest first.
  await box.fill(`Second note ${stamp}`);
  await box.press("Control+Enter");
  await expect(stream.getByTestId("note")).toHaveCount(2);
  await expect(stream.getByTestId("note").first().locator(".note-body")).toHaveText(`Second note ${stamp}`);

  // Edit the newest note.
  const newest = stream.getByTestId("note").first();
  await newest.getByRole("button", { name: "Edit" }).click();
  const editBox = newest.getByLabel("Note text");
  await expect(editBox).toBeFocused();
  await editBox.fill(`Second note ${stamp}, corrected`);
  await newest.getByRole("button", { name: "Save note" }).click();
  await expect(newest.locator(".note-body")).toHaveText(`Second note ${stamp}, corrected`);
  await expect(newest.getByText("(edited)")).toBeVisible();

  // Someone else saves the same note while it is open here: the save is refused, the saved text is shown, and
  // saving again replaces it.
  await newest.getByRole("button", { name: "Edit" }).click();
  await newest.getByLabel("Note text").fill(`Mine ${stamp}`);
  const [current] = await notesOf(request, ci.id);
  await apiSend(request, "PATCH", `/configuration-items/${ci.id}/notes/${current.id}`, { version: current.version, body: `Theirs ${stamp}` });
  await newest.getByRole("button", { name: "Save note" }).click();
  await expect(newest.getByText("Someone changed this note while you were editing.")).toBeVisible();
  await expect(newest.locator(".note-current")).toHaveText(`Theirs ${stamp}`);
  await newest.getByRole("button", { name: "Replace saved text" }).click();
  await expect(newest.locator(".note-body")).toHaveText(`Mine ${stamp}`);
  expect((await notesOf(request, ci.id))[0].body).toBe(`Mine ${stamp}`);

  // Delete asks first and quotes the note.
  await newest.getByRole("button", { name: "Delete" }).click();
  const dialog = page.getByRole("dialog", { name: "Delete this note?" });
  await expect(dialog).toContainText(`Mine ${stamp}`);
  await dialog.getByRole("button", { name: "Delete note" }).click();
  await expect(dialog).toBeHidden();
  await expect(stream.getByTestId("note")).toHaveCount(1);
  await expect(page.getByRole("tab", { name: "Notes 1" })).toBeVisible();
  expect((await notesOf(request, ci.id)).map((n) => n.body)).toEqual(["Maintenance window: Sun 02:00-04:00\nCall **ops** <b>first</b>"]);

  // Every write is in the audit log as a CI note.
  const audit = await apiGet<{ data: { entityType: string; action: string }[] }>(request, `/audit-log?entityType=ci_notes&limit=200`);
  const actions = audit.data.filter((e) => e.entityType === "ci_notes").map((e) => e.action);
  for (const a of ["create", "update", "delete"]) expect(actions).toContain(a);
});

test.describe("permission-aware actions", () => {
  const USERNAME = `e2e-notes-${stamp}`;
  const PASSWORD = "notes-e2e-password-123";
  let appCi: { id: string };
  let serverCi: { id: string };

  async function signInUi(browser: Browser): Promise<Page> {
    const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
    const page = await context.newPage();
    await page.goto("/login");
    await page.getByLabel("Username").fill(USERNAME);
    await page.getByLabel("Password").fill(PASSWORD);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
    return page;
  }

  test.beforeAll(async ({ request }) => {
    const appId = await classIdByName(request, "Application");
    const serverId = await classIdByName(request, "Server");
    // Applications: view and edit. Servers: view only.
    const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: `E2E notes ${stamp}`,
      globalPermissions: [],
      classPermissions: [
        { classId: appId, view: true, create: false, edit: true, delete: false },
        { classId: serverId, view: true, create: false, edit: false, delete: false },
      ],
    });
    await apiSend(request, "POST", "/admin/users", {
      username: USERNAME,
      email: `${USERNAME}@example.test`,
      displayName: `E2E Notes ${stamp}`,
      password: PASSWORD,
      profileIds: [profile.id],
    });
    appCi = await createCi(request, appId, `zz-notes-app-${stamp}`);
    serverCi = await createCi(request, serverId, `zz-notes-srv-${stamp}`, { hostname: `zz-notes-${stamp}` });
    await apiSend(request, "POST", `/configuration-items/${appCi.id}/notes`, { body: `Admin on app ${stamp}` });
    await apiSend(request, "POST", `/configuration-items/${serverCi.id}/notes`, { body: `Admin on server ${stamp}` });
  });

  test("a viewer reads notes but gets no add box and no actions", async ({ browser }) => {
    const page = await signInUi(browser);
    await openNotes(page, serverCi.id);
    const panel = page.getByRole("region", { name: /^Notes/ });
    await expect(panel.getByText(`Admin on server ${stamp}`)).toBeVisible();
    await expect(panel.getByLabel("Add a note")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Edit" })).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Delete" })).toHaveCount(0);
    await page.context().close();
  });

  test("an editor changes only their own notes; an administrator may delete them but not edit them", async ({ browser, page, request }) => {
    const user = await signInUi(browser);
    await openNotes(user, appCi.id);
    const panel = user.getByRole("region", { name: /^Notes/ });
    const adminNote = panel.getByTestId("note").filter({ hasText: `Admin on app ${stamp}` });
    await expect(adminNote).toBeVisible();
    await expect(adminNote.getByRole("button")).toHaveCount(0);

    await panel.getByLabel("Add a note").fill(`User on app ${stamp}`);
    await panel.getByRole("button", { name: "Add note" }).click();
    const own = panel.getByTestId("note").filter({ hasText: `User on app ${stamp}` });
    await expect(own.getByRole("button", { name: "Edit" })).toBeVisible();
    await expect(own.getByRole("button", { name: "Delete" })).toBeVisible();

    // The server stays authoritative: a forged edit of the administrator's note is refused.
    const adminRow = (await notesOf(request, appCi.id)).find((n) => n.body === `Admin on app ${stamp}`)!;
    const token = (await user.context().cookies()).find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
    const refused = await user.request.fetch(`/api/v1/configuration-items/${appCi.id}/notes/${adminRow.id}`, {
      method: "PATCH",
      data: { version: adminRow.version, body: "forged" },
      headers: { "X-CSRF-Token": token },
    });
    expect(refused.status()).toBe(403);
    await user.context().close();

    // The administrator sees Delete on the user's note, not Edit.
    await openNotes(page, appCi.id);
    const theirs = page.getByRole("region", { name: /^Notes/ }).getByTestId("note").filter({ hasText: `User on app ${stamp}` });
    await expect(theirs.getByRole("button", { name: "Delete" })).toBeVisible();
    await expect(theirs.getByRole("button", { name: "Edit" })).toHaveCount(0);
  });
});
