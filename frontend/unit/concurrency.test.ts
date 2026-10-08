// Unit tests for the concurrency limit the rail's saved-view counts go through (GH#780): the server counts for
// only a few requests at once, so 150+ views (4 batches of 50) must not be sent all together.
// Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { concurrencyLimit } from "../src/lib/concurrency";

function deferred() {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => ((resolve = res), (reject = rej)));
  return { promise, resolve, reject };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe("concurrencyLimit", () => {
  test("runs at most 2 of 4 batches at once, in call order", async () => {
    const limit = concurrencyLimit(2);
    const gates = [deferred(), deferred(), deferred(), deferred()];
    const started: number[] = [];
    let inFlight = 0;
    let peak = 0;
    const runs = gates.map((g, i) =>
      limit(async () => {
        started.push(i);
        peak = Math.max(peak, ++inFlight);
        await g.promise;
        inFlight--;
        return i;
      }),
    );
    await tick();
    assert.deepEqual(started, [0, 1]);
    gates[1].resolve();
    await tick();
    assert.deepEqual(started, [0, 1, 2]);
    // A call made while the slot is handed over still waits its turn.
    const late = limit(async () => (started.push(4), 4));
    gates[0].resolve();
    await tick();
    assert.deepEqual(started, [0, 1, 2, 3]);
    gates[2].resolve();
    gates[3].resolve();
    assert.deepEqual(await Promise.all([...runs, late]), [0, 1, 2, 3, 4]);
    assert.deepEqual(started, [0, 1, 2, 3, 4]);
    assert.equal(peak, 2);
  });

  test("a failed batch frees its slot and keeps its error", async () => {
    const limit = concurrencyLimit(1);
    const failing = limit(() => Promise.reject(new Error("boom")));
    const next = limit(async () => "ok");
    await assert.rejects(failing, /boom/);
    assert.equal(await next, "ok");
  });

  test("a batch cancelled while it waits is never sent", async () => {
    const limit = concurrencyLimit(1);
    const gate = deferred();
    const first = limit(() => gate.promise);
    const ctl = new AbortController();
    let sent = false;
    const queued = limit(async () => {
      ctl.signal.throwIfAborted();
      sent = true;
    });
    ctl.abort();
    gate.resolve();
    await first;
    await assert.rejects(queued, { name: "AbortError" });
    assert.equal(sent, false);
    assert.equal(await limit(async () => "free"), "free");
  });
});
