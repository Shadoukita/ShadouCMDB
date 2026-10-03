// The toast queue behind ToastHost: timing, the stack limit and pausing while focused.
import assert from "node:assert/strict";
import { afterEach, beforeEach, describe, mock, test } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { TOAST_MAX, TOAST_MS, useFlashStore } from "../src/stores/flash";

const texts = () => useFlashStore().toasts.map((t) => t.text);

describe("toasts", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mock.timers.enable({ apis: ["setTimeout"] });
  });
  afterEach(() => mock.timers.reset());

  test("a toast closes after its time", () => {
    const flash = useFlashStore();
    flash.show("Created crm-app-01.");
    mock.timers.tick(TOAST_MS - 1);
    assert.deepEqual(texts(), ["Created crm-app-01."]);
    mock.timers.tick(1);
    assert.deepEqual(texts(), []);
  });

  test("each toast keeps its own clock and can be dismissed", () => {
    const flash = useFlashStore();
    const first = flash.show("one");
    mock.timers.tick(TOAST_MS / 2);
    flash.show("two");
    flash.show("three");
    flash.dismiss(first);
    assert.deepEqual(texts(), ["two", "three"]);
    mock.timers.tick(TOAST_MS / 2);
    assert.deepEqual(texts(), ["two", "three"]);
    mock.timers.tick(TOAST_MS / 2);
    assert.deepEqual(texts(), []);
  });

  test(`at most ${TOAST_MAX} toasts are on screen, the oldest goes first`, () => {
    const flash = useFlashStore();
    for (let i = 1; i <= TOAST_MAX + 2; i++) flash.show(`t${i}`);
    assert.deepEqual(texts(), Array.from({ length: TOAST_MAX }, (_, i) => `t${i + 3}`));
  });

  test("focus pauses the clocks; leaving gives each toast its full time again", () => {
    const flash = useFlashStore();
    flash.show("one");
    mock.timers.tick(TOAST_MS - 10);
    flash.pause();
    flash.show("two");
    mock.timers.tick(TOAST_MS * 3);
    assert.deepEqual(texts(), ["one", "two"]);
    flash.resume();
    mock.timers.tick(TOAST_MS - 1);
    assert.deepEqual(texts(), ["one", "two"]);
    mock.timers.tick(1);
    assert.deepEqual(texts(), []);
  });
});
