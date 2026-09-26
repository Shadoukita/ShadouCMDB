import { test as base, expect, type APIRequestContext, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { join } from "node:path";

export { expect };

/**
 * Every test fails on uncaught page errors, console errors and Vue warnings.
 * "Failed to load resource" is excluded: the tests provoke 4xx responses on purpose.
 */
export const test = base.extend<{ failOnPageErrors: void }>({
  failOnPageErrors: [
    async ({ page }, use) => {
      const problems: string[] = [];
      page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
      page.on("console", (m) => {
        const text = m.text();
        if (text.startsWith("Failed to load resource")) return;
        if (m.type() === "error" || text.includes("[Vue warn]")) problems.push(`${m.type()}: ${text}`);
      });
      await use();
      expect(problems, "page errors / Vue warnings").toEqual([]);
    },
    { auto: true },
  ],
});

/** Saves a full-page screenshot when E2E_SCREENSHOT_DIR is set (evidence for reviews). */
export async function snap(page: Page, name: string) {
  const dir = process.env.E2E_SCREENSHOT_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  await page.screenshot({ path: join(dir, `${name}.png`), fullPage: true });
}

/** Direct API access through the UI origin (same /api/v1 the app uses). */
export async function apiGet<T>(request: APIRequestContext, path: string): Promise<T> {
  const res = await request.get(`/api/v1${path}`);
  expect(res.ok(), `GET ${path} → ${res.status()}`).toBeTruthy();
  return (await res.json()) as T;
}

export async function apiSend<T>(request: APIRequestContext, method: "POST" | "PATCH", path: string, data: unknown): Promise<T> {
  const res = await request.fetch(`/api/v1${path}`, { method, data, headers: { "X-Actor-Name": "e2e" } });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as T;
}

interface Page_<T> {
  data: T[];
}

export async function classIdByName(request: APIRequestContext, name: string): Promise<string> {
  const classes = await apiGet<Page_<{ id: string; name: string }>>(request, "/ci-classes?limit=200");
  const cls = classes.data.find((c) => c.name === name);
  expect(cls, `class ${name}`).toBeTruthy();
  return cls!.id;
}

export async function ciIdByName(request: APIRequestContext, name: string): Promise<string> {
  const list = await apiGet<Page_<{ id: string; name: string }>>(request, `/configuration-items?q=${encodeURIComponent(name)}&limit=50`);
  const ci = list.data.find((c) => c.name === name);
  expect(ci, `CI ${name}`).toBeTruthy();
  return ci!.id;
}

/** Picks a CI in a CiPicker combobox by typing and clicking the option whose name matches exactly. */
export async function pickCi(page: Page, inputSelector: string, search: string, name: string) {
  await page.locator(inputSelector).fill(search);
  const option = page.getByRole("option").filter({ has: page.getByText(name, { exact: true }) });
  await option.first().click();
}
