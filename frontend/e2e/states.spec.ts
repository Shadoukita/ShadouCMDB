import type { Route } from "@playwright/test";
import { snap, expect, test } from "./support";

test("unknown and malformed CI ids show a designed not-found state", async ({ page }) => {
  await page.goto("/cis/00000000-0000-4000-8000-000000000000");
  await expect(page.getByRole("heading", { name: "Configuration item not found" })).toBeVisible();
  await page.goto("/cis/not-a-uuid");
  await expect(page.getByRole("heading", { name: "Configuration item not found" })).toBeVisible();
  await page.goto("/no/such/screen");
  await expect(page.getByRole("heading", { name: "Page not found" })).toBeVisible();
});

test("an empty inventory tells the operator how to create the first CI", async ({ page }) => {
  await page.route("**/api/v1/configuration-items?*", (route) =>
    route.fulfill({ json: { data: [], page: { limit: 50, offset: 0, total: 0 } } }),
  );
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /the inventory is empty/ })).toBeVisible();
  await page.goto("/cis");
  await expect(page.getByRole("heading", { name: "The inventory is empty" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Create your first configuration item" })).toHaveAttribute("href", "/cis/new");
  await snap(page, "15-empty-inventory");
});

test("an unreachable API is explained, not a blank page", async ({ page }) => {
  await page.route("**/api/v1/**", (route) => route.abort("connectionrefused"));
  await page.goto("/cis");
  const alert = page.getByRole("alert").first();
  await expect(alert).toContainText("API unreachable", { timeout: 15_000 });
  await expect(alert).toContainText("Cannot reach the ShadouCMDB API");
  await expect(alert.getByRole("button", { name: "Retry" })).toBeVisible();
  await snap(page, "16-api-unreachable");
});

test("an unmigrated database says to run migrate, and Retry recovers once it has run", async ({ page }) => {
  const message = "The database schema is not migrated (0 of 13 migrations applied). Run `shadoucmdb migrate`, then retry.";
  const notMigrated = (route: Route) =>
    route.fulfill({ status: 503, json: { error: { code: "SCHEMA_NOT_MIGRATED", message, requestId: "e2e" } } });
  await page.route("**/api/v1/**", notMigrated);
  await page.goto("/cis");
  const alert = page.getByRole("alert").first();
  await expect(alert).toContainText("The database is not migrated yet");
  await expect(alert).toContainText("Run `shadoucmdb migrate`, then retry.");
  await expect(page).toHaveURL(/\/cis$/); // not sent to sign-in or setup
  await snap(page, "16b-schema-not-migrated");
  await page.unroute("**/api/v1/**", notMigrated);
  await alert.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByRole("heading", { name: "Configuration items" })).toBeVisible();
});
