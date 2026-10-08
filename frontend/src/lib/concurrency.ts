/**
 * Runs at most `max` of the given tasks at once; the rest wait in call order. A finished task hands its slot
 * straight to the next waiting one, so a call made in between cannot slip past the limit.
 */
export function concurrencyLimit(max: number) {
  let active = 0;
  const waiting: (() => void)[] = [];
  return async function run<T>(task: () => Promise<T>): Promise<T> {
    if (active < max) active++;
    else await new Promise<void>((resolve) => waiting.push(resolve));
    try {
      return await task();
    } finally {
      const next = waiting.shift();
      if (next) next();
      else active--;
    }
  };
}
