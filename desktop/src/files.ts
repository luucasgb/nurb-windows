// What General > Files shows, as the shell reports it (src-tauri/src/files.rs).
export type StorageLocations = {
  library: { path: string; bytes: number | null };
  exports: { path: string; custom: boolean; missing: string | null };
  appData: { path: string; bytes: number | null };
};

const UNITS = ["KB", "MB", "GB", "TB"];

/** A size the way the file manager beside it writes one: "20 MB", "1.2 GB".
 * Finder counts in 1000s, Explorer in 1024s under the same names. */
export function formatBytes(bytes: number, base: 1000 | 1024 = 1000): string {
  if (bytes < base) return `${bytes} bytes`;
  let value = bytes / base;
  let unit = 0;
  const rounded = (v: number) => (v < 10 ? Math.round(v * 10) / 10 : Math.round(v));
  // Rounding can carry into the next unit: 999,999 bytes is 1 MB, not 1000 KB.
  while (rounded(value) >= base && unit < UNITS.length - 1) {
    value /= base;
    unit += 1;
  }
  return `${rounded(value)} ${UNITS[unit]}`;
}
