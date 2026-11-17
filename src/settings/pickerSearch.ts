/** How long after an empty answer the picker asks once more. */
export const REASK_MS = 1000;

/**
 * Asks `search` for `text` and hands the rows to `show`. The first ask of
 * `connect`, `wifi x` or `eject` may only start reading the device list, so an
 * empty answer is asked again once, `reaskMs` later. Returns a cancel
 * function: nothing is shown or asked after it is called.
 */
export function askItems<T>(
  text: string,
  search: (text: string) => Promise<T[]>,
  show: (rows: T[]) => void,
  reaskMs = REASK_MS,
): () => void {
  let cancelled = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const ask = (again: boolean) => {
    search(text).then(
      (rows) => {
        if (cancelled) return;
        show(rows);
        if (rows.length === 0 && again) timer = setTimeout(() => ask(false), reaskMs);
      },
      () => {
        if (!cancelled) show([]);
      },
    );
  };
  ask(true);
  return () => {
    cancelled = true;
    if (timer !== undefined) clearTimeout(timer);
  };
}
