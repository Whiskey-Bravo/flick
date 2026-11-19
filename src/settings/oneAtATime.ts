/**
 * Runs one save at a time. A call while a save is on its way (a double-click
 * on Save) runs nothing and answers false. `busy` hears true when a save
 * starts and false when it ends, so the Save buttons can be off meanwhile.
 */
export function oneAtATime(busy: (on: boolean) => void): (save: () => Promise<boolean>) => Promise<boolean> {
  let running = false;
  return async (save) => {
    if (running) return false;
    running = true;
    busy(true);
    try {
      return await save();
    } finally {
      running = false;
      busy(false);
    }
  };
}
