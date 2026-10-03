// The flash store behind the shell's toasts (SHAA-1734, audit F1): queueing, dismiss, and errors
// that stay until dismissed. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, beforeEach, describe, mock, test } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { MAX_TOASTS, SUCCESS_TTL_MS, useFlashStore } from "../src/stores/flash";

const texts = (s: ReturnType<typeof useFlashStore>) => s.toasts.map((x) => `${x.tone}:${x.text}`);

describe("flash store → toasts", () => {
  beforeEach(() => {
    mock.timers.enable({ apis: ["setTimeout", "Date"] });
    setActivePinia(createPinia());
  });
  afterEach(() => mock.timers.reset());

  test("toasts queue oldest first, each with its own id", () => {
    const s = useFlashStore();
    const a = s.success("Created crm-app-01.");
    const b = s.error("Could not reach the directory.");
    const c = s.success("Saved crm-app-01.");
    assert.deepEqual(texts(s), ["success:Created crm-app-01.", "danger:Could not reach the directory.", "success:Saved crm-app-01."]);
    assert.equal(new Set([a, b, c]).size, 3);
  });

  test("a success toast closes after the TTL; an error stays until dismissed", () => {
    const s = useFlashStore();
    s.success("Saved.");
    const err = s.error("Failed.");
    mock.timers.tick(SUCCESS_TTL_MS - 1);
    assert.equal(s.toasts.length, 2);
    mock.timers.tick(1);
    assert.deepEqual(texts(s), ["danger:Failed."]);
    mock.timers.tick(10 * SUCCESS_TTL_MS);
    assert.deepEqual(texts(s), ["danger:Failed."]);
    s.dismiss(err);
    assert.deepEqual(s.toasts, []);
  });

  test("dismiss removes one toast and stops its timer", () => {
    const s = useFlashStore();
    const a = s.success("A.");
    s.success("B.");
    s.dismiss(a);
    assert.deepEqual(texts(s), ["success:B."]);
    s.dismiss(a); // already gone: no-op
    mock.timers.tick(SUCCESS_TTL_MS);
    assert.deepEqual(s.toasts, []);
  });

  test("the same message again restarts the toast instead of stacking a copy", () => {
    const s = useFlashStore();
    const a = s.success("Saved.");
    mock.timers.tick(SUCCESS_TTL_MS - 1000);
    assert.equal(s.success("Saved."), a);
    assert.equal(s.toasts.length, 1);
    mock.timers.tick(SUCCESS_TTL_MS - 1);
    assert.equal(s.toasts.length, 1);
    mock.timers.tick(1);
    assert.equal(s.toasts.length, 0);
  });

  test("past the limit the oldest success makes room; errors are never dropped", () => {
    const s = useFlashStore();
    s.error("E1.");
    for (let i = 1; i <= MAX_TOASTS; i++) s.success(`S${i}.`);
    assert.equal(s.toasts.length, MAX_TOASTS);
    assert.deepEqual(texts(s), ["danger:E1.", ...Array.from({ length: MAX_TOASTS - 1 }, (_, i) => `success:S${i + 2}.`)]);
    for (let i = 2; i <= MAX_TOASTS + 2; i++) s.error(`E${i}.`);
    assert.ok(s.toasts.every((x) => x.tone === "danger"));
    assert.equal(s.toasts.length, MAX_TOASTS + 2);
  });

  test("pause holds a success toast while it is read; resume continues with the time left", () => {
    const s = useFlashStore();
    const a = s.success("Saved.");
    mock.timers.tick(SUCCESS_TTL_MS - 3000);
    s.pause(a);
    mock.timers.tick(10 * SUCCESS_TTL_MS);
    assert.equal(s.toasts.length, 1);
    s.resume(a);
    mock.timers.tick(2999);
    assert.equal(s.toasts.length, 1);
    mock.timers.tick(1);
    assert.equal(s.toasts.length, 0);
  });

  test("resume never leaves less than two seconds to read", () => {
    const s = useFlashStore();
    const a = s.success("Saved.");
    mock.timers.tick(SUCCESS_TTL_MS - 100);
    s.pause(a);
    s.resume(a);
    mock.timers.tick(1999);
    assert.equal(s.toasts.length, 1);
    mock.timers.tick(1);
    assert.equal(s.toasts.length, 0);
  });

  test("pause and resume do not give an error toast a timer", () => {
    const s = useFlashStore();
    const e = s.error("Failed.");
    s.pause(e);
    s.resume(e);
    mock.timers.tick(10 * SUCCESS_TTL_MS);
    assert.equal(s.toasts.length, 1);
  });
});
