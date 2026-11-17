// Writes the updater's latest.json for a release.
// Usage: node scripts/latest-json.mjs <version> <notes-file> <sig-file> > latest.json
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export function buildLatestJson(version, notes, sig, now) {
  return {
    version,
    notes,
    pub_date: now.toISOString(),
    platforms: {
      "windows-x86_64": {
        signature: sig,
        url: `https://github.com/Whiskey-Bravo/flick/releases/download/v${version}/Flick_${version}_x64-setup.exe`,
      },
    },
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [version, notesFile, sigFile] = process.argv.slice(2);
  if (!version || !notesFile || !sigFile) {
    console.error("usage: node scripts/latest-json.mjs <version> <notes-file> <sig-file> > latest.json");
    process.exit(1);
  }
  const notes = readFileSync(notesFile, "utf8").trim();
  const sig = readFileSync(sigFile, "utf8").trim();
  process.stdout.write(JSON.stringify(buildLatestJson(version, notes, sig, new Date()), null, 2) + "\n");
}
