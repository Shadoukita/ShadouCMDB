import { apiGet, apiSend, checkA11y, ciIdByName, classIdByName, createCi, csrf, expect, lookupValueId, test } from "./support";

// The History tab's point-in-time view and compare (SHAA-3131): the CI as of a past moment and the net
// difference between two moments, rebuilt in the browser from GET /audit-log?entityId=…. The admin has audit.view.

const created: string[] = [];

test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of created.splice(0)) {
    const res = await request.delete(`/api/v1/configuration-items/${id}`, { headers });
    expect(res.ok(), `delete ${id} → ${res.status()}`).toBeTruthy();
  }
});

type Ci = { id: string; version: number };

test("history: view a CI as of a change, compare two changes, and say when it did not exist yet", async ({ page, request }, testInfo) => {
  const stamp = `e2e-pit-${Date.now().toString(36)}`;
  const serverId = await classIdByName(request, "Server");
  const maintenance = await lookupValueId(request, "status", "maintenance");
  // Created with 8 GB in service, then 16 GB in maintenance, then renamed.
  const ci = await createCi(request, serverId, `${stamp}-a`, { memory_gb: 8 });
  created.push(ci.id);
  const v1 = await apiSend<Ci>(request, "PATCH", `/configuration-items/${ci.id}`, { version: ci.version, attributes: { memory_gb: 16, status: maintenance } });
  await apiSend<Ci>(request, "PATCH", `/configuration-items/${ci.id}`, { version: v1.version, attributes: { name: `${stamp}-b` } });
  const log = await apiGet<{ data: { occurredAt: string; action: string }[] }>(request, `/audit-log?entityId=${ci.id}&sort=occurredAt&limit=10`);
  expect(log.data.map((e) => e.action)).toEqual(["create", "update", "update"]);

  await page.goto(`/cis/${ci.id}`);
  await page.getByRole("tab", { name: /^History/ }).click();
  const views = page.getByRole("group", { name: "History view" });
  await expect(views.getByRole("button", { name: "Timeline" })).toHaveAttribute("aria-pressed", "true");
  const entries = page.locator("ol.event-timeline > li");
  await expect(entries).toHaveCount(3);

  // As of the creation (the oldest entry, last in the timeline): 8 GB, in service, the first name.
  const createdAt = log.data[0].occurredAt.slice(0, 19).replace("T", " ");
  await entries.last().getByRole("button", { name: new RegExp(`^View the CI as of this change, ${createdAt}`) }).click();
  await expect(views.getByRole("button", { name: "As of" })).toHaveAttribute("aria-pressed", "true");
  const asOf = page.locator(".history-pit-body");
  await expect(asOf.getByRole("heading", { name: new RegExp(`^As of ${createdAt} UTC`) })).toBeVisible();
  await expect(asOf.getByText("Historical, read-only")).toBeVisible();
  await expect(asOf.locator("dd[data-field='attributes.memory_gb']")).toHaveText("8");
  await expect(asOf.locator("dd[data-field='attributes.status']")).toHaveText("In service");
  await expect(asOf.locator("dd[data-field='attributes.name']")).toHaveText(`${stamp}-a`);
  await expect(page.getByLabel("View as of (UTC)")).toHaveValue(log.data[0].occurredAt.slice(0, 16));
  await checkA11y(page, testInfo, "history-as-of", { include: ".history-pit" });

  // Back to the current state: the timeline again.
  await page.getByRole("button", { name: "Back to current state" }).click();
  await expect(entries).toHaveCount(3);

  // Compare the two updates: from just before the first to just after the second, the net change per field.
  await entries.nth(0).getByRole("checkbox", { name: /^Select the change of .* for comparison$/ }).check();
  await expect(page.getByRole("button", { name: "Compare selected (1 of 2)" })).toBeDisabled();
  await entries.nth(1).getByRole("checkbox", { name: /^Select the change of .* for comparison$/ }).check();
  await page.getByRole("button", { name: "Compare selected (2 of 2)" }).click();
  await expect(views.getByRole("button", { name: "Compare" })).toHaveAttribute("aria-pressed", "true");
  const diff = page.getByRole("list", { name: "Field differences" });
  await expect(diff.locator("li")).toHaveCount(3);
  await expect(diff.locator("li[data-field='attributes.name']")).toContainText(`${stamp}-a`);
  await expect(diff.locator("li[data-field='attributes.name'] ins")).toHaveText(`${stamp}-b`);
  await expect(diff.locator("li[data-field='attributes.memory_gb'] del")).toHaveText("8");
  await expect(diff.locator("li[data-field='attributes.memory_gb'] ins")).toHaveText("16");
  await expect(diff.locator("li[data-field='attributes.status'] del")).toHaveText("In service");
  await expect(diff.locator("li[data-field='attributes.status'] ins")).toHaveText("Maintenance");
  await expect(page.getByText("2 recorded changes in this range.")).toBeVisible();
  await checkA11y(page, testInfo, "history-compare", { include: ".history-pit" });

  // From a moment before the CI existed: it says so, and the values at the later moment show as added.
  await page.getByLabel("From (UTC)").fill("2020-01-01T00:00");
  await page.getByLabel("From (UTC)").dispatchEvent("change");
  await expect(page.getByText(/^At 2020-01-01 00:00:00 UTC the CI did not exist yet/)).toBeVisible();
  await expect(diff.locator("li[data-field='attributes.memory_gb'] ins")).toHaveText("16");

  // As of a moment before the CI existed, typed into the picker.
  await views.getByRole("button", { name: "As of" }).click();
  await page.getByLabel("View as of (UTC)").fill("2020-01-01T00:00");
  await page.getByLabel("View as of (UTC)").dispatchEvent("change");
  await expect(page.getByText(/^The CI did not exist yet\. It was created at /)).toBeVisible();
});

test("history: an auditor who may not view a referenced CI's class sees it as hidden, as of any moment", async ({ browser, request }, testInfo) => {
  const stamp = `e2e-pit-${Date.now().toString(36)}`;
  const applicationId = await classIdByName(request, "Application");
  // An Application referring to crm-db (a Database), then renamed, by the administrator.
  const crmDb = await ciIdByName(request, "crm-db");
  const app = await createCi(request, applicationId, `${stamp}-app`, { primary_database: crmDb });
  created.push(app.id);
  await apiSend(request, "PATCH", `/configuration-items/${app.id}`, { version: app.version, attributes: { name: `${stamp}-app-2`, primary_database: null } });
  // The auditor has audit.view and may view Applications only.
  const PASSWORD = "auditor-password-123";
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E history auditors ${stamp}`,
    globalPermissions: ["audit.view"],
    classPermissions: [{ classId: applicationId, view: true, create: false, edit: false, delete: false }],
  });
  const username = `${stamp}-auditor`;
  await apiSend(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName: `E2E History Auditor ${stamp}`, password: PASSWORD, profileIds: [profile.id] });

  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await page.goto(`/cis/${app.id}`);
  await page.getByRole("tab", { name: /^History/ }).click();
  const entries = page.locator("ol.event-timeline > li");
  await expect(entries).toHaveCount(2);

  // As of the creation the reference was set, into a class the auditor may not view: "hidden", never its name or id.
  await entries.last().getByRole("button", { name: /^View the CI as of this change/ }).click();
  const asOf = page.locator(".history-pit-body");
  await expect(asOf.locator("dd[data-field='attributes.name']")).toHaveText(`${stamp}-app`);
  await expect(asOf.locator("dd[data-field='attributes.primary_database']")).toHaveText("hidden");
  await expect(page.locator(".history-pit")).not.toContainText("crm-db");
  expect(await page.locator(".history-pit").innerHTML()).not.toContain(crmDb);
  await checkA11y(page, testInfo, "history-as-of-hidden", { include: ".history-pit" });

  // Compared with now: the visible side only.
  await page.getByRole("group", { name: "History view" }).getByRole("button", { name: "Compare" }).click();
  await page.getByLabel("From (UTC)").fill("2020-01-01T00:00");
  await page.getByLabel("From (UTC)").dispatchEvent("change");
  await expect(page.getByRole("list", { name: "Field differences" })).toBeVisible();
  await expect(page.locator(".history-pit")).not.toContainText("crm-db");
  expect(await page.locator(".history-pit").innerHTML()).not.toContain(crmDb);
  await context.close();
});
