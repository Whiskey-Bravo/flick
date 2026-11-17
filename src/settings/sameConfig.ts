/** True when two settings states hold the same config, so a reload that changed nothing is not news. */
export function sameConfig(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
