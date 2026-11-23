//! Where the app keeps what it writes, for General > Files: the library the
//! engine owns, the folder exports are saved to, and the app's own data.
//!
//! Exports default to Downloads. A chosen folder is remembered in the data
//! dir and granted to the page's file writes at runtime, because the
//! capability file can only name folders known when the app was built.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tauri_plugin_fs::FsExt;
use tauri_plugin_opener::OpenerExt;

/// The chosen export folder, one path. Absent means Downloads.
const EXPORT_FILE: &str = "export-folder";

/// The apps Open in slicer may hand a file to: the same three the capability
/// file names for the page's own opener calls.
const SLICERS: [&str; 3] = ["BambuStudio", "OrcaSlicer", "PrusaSlicer"];

pub struct Files {
    data: PathBuf,
    export: Mutex<Option<PathBuf>>,
}

impl Files {
    pub fn load(data: &Path) -> Self {
        let export = std::fs::read_to_string(data.join(EXPORT_FILE))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(PathBuf::from);
        Self {
            data: data.to_path_buf(),
            export: Mutex::new(export),
        }
    }

    fn chosen(&self) -> Option<PathBuf> {
        self.export.lock().unwrap().clone()
    }
}

/// Lets the page write into the chosen folder: at startup, on every change, and
/// when an export finds a folder that was missing at startup.
pub fn grant(app: &AppHandle, files: &Files) {
    if let Some(dir) = files.chosen().filter(|dir| dir.is_dir()) {
        let _ = app.fs_scope().allow_directory(&dir, true);
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    path: String,
    /// None while it is unknown or too slow to count here.
    bytes: Option<u64>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exports {
    path: String,
    /// A folder the person chose, rather than Downloads.
    custom: bool,
    /// The chosen folder, when it is gone (a drive unplugged): exports go to
    /// Downloads until it is back or another is chosen.
    missing: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Locations {
    library: Location,
    exports: Exports,
    app_data: Location,
}

fn downloads(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .download_dir()
        .map_err(|e| format!("could not find the Downloads folder: {e}"))
}

/// Where an export lands now, and what the Files row says about it.
fn exports(app: &AppHandle, files: &Files) -> Result<(PathBuf, Exports), String> {
    match files.chosen() {
        Some(dir) if dir.is_dir() => Ok((
            dir.clone(),
            Exports {
                path: dir.to_string_lossy().into_owned(),
                custom: true,
                missing: None,
            },
        )),
        chosen => {
            let dir = downloads(app)?;
            Ok((
                dir.clone(),
                Exports {
                    path: dir.to_string_lossy().into_owned(),
                    custom: false,
                    missing: chosen.map(|gone| gone.to_string_lossy().into_owned()),
                },
            ))
        }
    }
}

/// The database and its write-ahead files: the library itself. The serve's
/// scratch folder is rebuilt from it, so it does not count.
fn library_bytes(home: &Path) -> u64 {
    ["nurb.db", "nurb.db-wal", "nurb.db-shm"]
        .iter()
        .filter_map(|name| std::fs::metadata(home.join(name)).ok())
        .map(|meta| meta.len())
        .sum()
}

fn tree_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            // Links are counted where they point, if at all, never followed.
            Ok(kind) if kind.is_dir() => tree_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn app_data(app: &AppHandle) -> PathBuf {
    app.state::<crate::AppData>().0.clone()
}

#[tauri::command]
pub async fn storage_locations(app: AppHandle) -> Result<Locations, String> {
    let home = app.state::<crate::supervisor::Supervisor>().home();
    let (_, exports) = exports(&app, &app.state::<Files>())?;
    Ok(Locations {
        library: Location {
            bytes: Some(library_bytes(&home)),
            path: home.to_string_lossy().into_owned(),
        },
        exports,
        app_data: Location {
            path: app_data(&app).to_string_lossy().into_owned(),
            bytes: None,
        },
    })
}

/// The app data folder's size, asked for separately: the runtimes in it are
/// tens of thousands of files, and the rows should not wait on them.
#[tauri::command]
pub async fn app_data_size(app: AppHandle) -> u64 {
    let dir = app_data(&app);
    tauri::async_runtime::spawn_blocking(move || tree_bytes(&dir))
        .await
        .unwrap_or(0)
}

/// The folder the page writes an export into right now. A chosen folder on a
/// drive that was unplugged at startup was never granted, so it is granted
/// here, the first time an export finds it back.
#[tauri::command]
pub async fn export_folder(app: AppHandle) -> Result<String, String> {
    let files = app.state::<Files>();
    let (dir, exports) = exports(&app, &files)?;
    if exports.custom {
        grant(&app, &files);
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Asks for the export folder with the system's own picker, here rather than
/// in the page, so the page can never name a folder to be given: only the
/// person choosing one can. None when the picker was cancelled.
#[tauri::command]
pub async fn choose_export_folder(app: AppHandle) -> Result<Option<Exports>, String> {
    use tauri_plugin_dialog::DialogExt;
    let files = app.state::<Files>();
    let (current, _) = exports(&app, &files)?;
    let (picked_tx, picked_rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Save exports to")
        .set_directory(current);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    dialog.pick_folder(move |picked| {
        let _ = picked_tx.send(picked);
    });
    let Some(picked) = picked_rx.await.ok().flatten() else {
        return Ok(None);
    };
    let dir = picked
        .into_path()
        .map_err(|e| format!("could not read the chosen folder: {e}"))?;
    remember(&app, &files, Some(dir)).map(Some)
}

/// Exports go back to Downloads.
#[tauri::command]
pub async fn reset_export_folder(app: AppHandle) -> Result<Exports, String> {
    remember(&app, &app.state::<Files>(), None)
}

/// Saves the choice. A folder is tried with a real write first, so one the
/// app cannot write to is refused here rather than at the first export. A
/// folder chosen before stays writable to the page until the app quits: the
/// runtime scope can add folders but not take them back, and every folder in
/// it was picked by the person in the system's own picker.
fn remember(app: &AppHandle, files: &Files, dir: Option<PathBuf>) -> Result<Exports, String> {
    let file = files.data.join(EXPORT_FILE);
    match dir {
        Some(dir) => {
            check_writable(&dir)?;
            std::fs::write(&file, dir.to_string_lossy().as_bytes())
                .map_err(|e| format!("could not save the export folder: {e}"))?;
            *files.export.lock().unwrap() = Some(dir);
            grant(app, files);
        }
        None => {
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("could not save the export folder: {e}")),
            }
            *files.export.lock().unwrap() = None;
        }
    }
    exports(app, files).map(|(_, exports)| exports)
}

fn check_writable(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(format!("{} is not a folder.", dir.display()));
    }
    let probe = dir.join(format!(".nurb-write-check-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .and_then(|()| std::fs::remove_file(&probe))
        .map_err(|e| format!("nurb cannot save files in {}: {e}", dir.display()))
}

/// Opens one of the Files rows' folders. Which folder, not a path, so the
/// page cannot use this to open anything else.
#[tauri::command]
pub async fn show_location(app: AppHandle, which: String) -> Result<(), String> {
    let opener = app.opener();
    let shown = match which.as_str() {
        "library" => {
            let home = app.state::<crate::supervisor::Supervisor>().home();
            let db = home.join("nurb.db");
            if db.is_file() {
                opener.reveal_item_in_dir(db)
            } else {
                opener.open_path(home.to_string_lossy(), None::<&str>)
            }
        }
        "exports" => {
            let (dir, _) = exports(&app, &app.state::<Files>())?;
            opener.open_path(dir.to_string_lossy(), None::<&str>)
        }
        "appData" => opener.open_path(app_data(&app).to_string_lossy(), None::<&str>),
        other => return Err(format!("unknown location: {other}")),
    };
    shown.map_err(|e| format!("could not open the folder: {e}"))
}

/// Hands an export to a slicer. The page's own opener is scoped to Downloads
/// at build time, so a file in a chosen folder goes through here: only a
/// file inside the export folder, and only to one of the slicers.
#[tauri::command]
pub async fn open_in_slicer(app: AppHandle, path: String, slicer: String) -> Result<(), String> {
    if !SLICERS.contains(&slicer.as_str()) {
        return Err(format!("{slicer} is not a slicer nurb opens."));
    }
    let (dir, _) = exports(&app, &app.state::<Files>())?;
    if !inside(Path::new(&path), &dir) {
        return Err("only an export can be opened in the slicer".into());
    }
    app.opener()
        .open_path(path, Some(slicer))
        .map_err(|e| format!("could not open the slicer: {e}"))
}

/// Whether a file is inside a folder, judged on their real paths so `..` and
/// links cannot step out.
fn inside(file: &Path, dir: &Path) -> bool {
    match (file.canonicalize(), dir.canonicalize()) {
        (Ok(file), Ok(dir)) => file.is_file() && file.starts_with(dir),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{check_writable, inside, library_bytes, tree_bytes, Files, EXPORT_FILE};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nurb-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_chosen_folder_is_read_back_and_a_blank_file_means_downloads() {
        let data = scratch("load");
        assert_eq!(Files::load(&data).chosen(), None);
        std::fs::write(data.join(EXPORT_FILE), "  \n").unwrap();
        assert_eq!(Files::load(&data).chosen(), None);
        std::fs::write(data.join(EXPORT_FILE), "/somewhere/prints\n").unwrap();
        assert_eq!(Files::load(&data).chosen(), Some("/somewhere/prints".into()));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn the_library_is_the_database_and_its_write_ahead_files() {
        let home = scratch("library");
        std::fs::write(home.join("nurb.db"), [0u8; 100]).unwrap();
        std::fs::write(home.join("nurb.db-wal"), [0u8; 20]).unwrap();
        std::fs::create_dir_all(home.join("scratch")).unwrap();
        std::fs::write(home.join("scratch").join("part.py"), [0u8; 5000]).unwrap();
        assert_eq!(library_bytes(&home), 120);
        assert_eq!(tree_bytes(&home), 5120);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn only_a_real_writable_folder_can_take_exports() {
        let dir = scratch("writable");
        assert!(check_writable(&dir).is_ok());
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "the probe was left behind");
        assert!(check_writable(&dir.join("missing")).is_err());
        assert!(check_writable(std::path::Path::new("relative")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_slicer_only_gets_files_from_inside_the_export_folder() {
        let dir = scratch("inside");
        let exports = dir.join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        std::fs::write(exports.join("bracket.3mf"), b"x").unwrap();
        std::fs::write(dir.join("elsewhere.3mf"), b"x").unwrap();
        assert!(inside(&exports.join("bracket.3mf"), &exports));
        assert!(!inside(&exports.join("..").join("elsewhere.3mf"), &exports));
        assert!(!inside(&exports.join("missing.3mf"), &exports));
        assert!(!inside(&exports, &exports), "a folder is not an export");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
