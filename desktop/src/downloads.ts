import { invoke } from "@tauri-apps/api/core";
import { join } from "@tauri-apps/api/path";
import { exists, writeFile } from "@tauri-apps/plugin-fs";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

/** The name Finder gives the nth copy: `bracket 2.3mf`. */
export function numbered(name: string, n: number): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? `${name.slice(0, dot)} ${n}${name.slice(dot)}` : `${name} ${n}`;
}

/** What "Saved to …" and the Files rows call a folder: its own name, so
 * Downloads reads as Downloads. */
export function folderName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

// The shell and its plugins reject with a bare string; the part view shows an
// Error's message and only a generic line for anything else.
function failure(e: unknown): Error {
  return e instanceof Error ? e : new Error(String(e));
}

// An export lands in the folder chosen under Settings > General (Downloads
// until then), never over an earlier one.
async function writeExport(name: string, blob: Blob): Promise<{ path: string; folder: string }> {
  try {
    const dir = await invoke<string>("export_folder");
    let path = await join(dir, name);
    for (let n = 2; await exists(path); n++) path = await join(dir, numbered(name, n));
    await writeFile(path, new Uint8Array(await blob.arrayBuffer()));
    return { path, folder: folderName(dir) };
  } catch (e) {
    throw failure(e);
  }
}

// Finder shows the export, so the next step (into the slicer) starts from the file itself.
export async function saveExport(name: string, blob: Blob): Promise<string> {
  const { path, folder } = await writeExport(name, blob);
  await revealItemInDir(path).catch(() => {});
  return folder;
}

// Or the slicer takes it straight from there. The shell hands it over only to
// one of the slicers the engine can drive, and only a file from the export folder.
export async function openInSlicer(name: string, blob: Blob, app: string): Promise<string> {
  const { path, folder } = await writeExport(name, blob);
  await invoke("open_in_slicer", { path, slicer: app }).catch((e) => {
    throw failure(e);
  });
  return folder;
}
