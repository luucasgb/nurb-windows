mod acp;
mod agents;
mod claude;
mod crash;
mod env;
mod files;
mod naming;
mod platform;
mod prefs;
mod provision;
mod sessions;
mod supervisor;
mod v1;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use supervisor::{ServeInfo, Supervisor};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, RunEvent, State};

/// The app data directory, after the debug-only override. Commands that own
/// a directory on disk read it from here rather than resolving it again.
struct AppData(pub(crate) PathBuf);

/// Share links (`nurb://open/<id>`) macOS handed the app, until the page takes them.
/// A link that launches the app arrives before the page or the engine is up, so it
/// waits here rather than riding an event nobody is listening for yet.
#[derive(Default)]
struct OpenedLinks(std::sync::Mutex<Vec<String>>);

#[tauri::command]
fn take_open_links(links: State<OpenedLinks>) -> Vec<String> {
    std::mem::take(&mut *links.0.lock().unwrap())
}

fn opened_links(app: &AppHandle, links: Vec<String>) {
    use tauri::Emitter;
    let links: Vec<String> = links.into_iter().filter(|link| link.starts_with("nurb://")).collect();
    if links.is_empty() {
        return;
    }
    app.state::<OpenedLinks>().0.lock().unwrap().extend(links);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let _ = app.emit("open-links", ());
}

/// Windows creates the main window hidden (tauri.windows.conf.json). Created
/// visible, it sat blank at the system's default spot while WebView2 started,
/// then jumped to where the window-state plugin put it last time. Hidden, it is
/// placed and sized first and shown once, when its page has loaded. macOS
/// creates it visible, so setup disarms this there.
struct Reveal(AtomicBool);

impl Default for Reveal {
    fn default() -> Self {
        Self(AtomicBool::new(true))
    }
}

/// How long a hidden window waits for its page before it is shown anyway: a
/// page that never finishes loading must not leave the app running with no
/// window at all.
const REVEAL_DEADLINE: Duration = Duration::from_secs(5);

fn reveal(app: &AppHandle) {
    if !app.state::<Reveal>().0.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The serve every project in this window talks to. Blocking, because a cold
/// start pays for the OCCT import before it answers.
#[tauri::command]
async fn serve_info(app: AppHandle) -> Result<ServeInfo, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<Supervisor>().ensure())
        .await
        .map_err(|e| e.to_string())?
}

/// The tail of the serve's own log, for the screen that says the engine did
/// not start. Bounded so a runaway log cannot flood the page.
#[tauri::command]
fn serve_log(app: AppHandle) -> String {
    let text = std::fs::read_to_string(app.state::<Supervisor>().home().join("serve.log"))
        .unwrap_or_default();
    log_tail(&text, 4000).to_string()
}

fn log_tail(text: &str, keep: usize) -> &str {
    if text.len() <= keep {
        return text;
    }

    let mut cut = text.len() - keep;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    let start = text[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
    &text[start..]
}

/// A working directory for one project's chat agents. They have no nurb tools
/// yet, so they need somewhere of their own to run.
#[tauri::command]
fn project_dir(data: State<AppData>, project_id: String) -> Result<String, String> {
    // The id becomes a path segment, so anything that could climb out of the
    // projects folder is refused rather than sanitized.
    if project_id.is_empty()
        || !project_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("project ids are letters, digits, dashes and underscores".into());
    }
    let dir = data.0.join("projects").join(&project_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// The folders nurb 0.x listed, handed over once for the page to import.
#[tauri::command]
fn take_v1_folders(data: State<AppData>) -> Vec<String> {
    v1::take_folders(&data.0)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedChat {
    session_id: String,
    agent: String,
}

#[tauri::command]
fn saved_chat(
    sessions: State<sessions::SessionStore>,
    project_id: String,
    part: String,
) -> Option<SavedChat> {
    sessions
        .saved_chat(&project_id, &part)
        .map(|(session_id, agent)| SavedChat { session_id, agent })
}

#[tauri::command]
fn rename_part_chat(
    sessions: State<sessions::SessionStore>,
    project_id: String,
    was: String,
    name: String,
) {
    sessions.rename_part(&project_id, &was, &name);
}

#[tauri::command]
fn select_part_chat(
    sessions: State<sessions::SessionStore>,
    project_id: String,
    part: String,
    session_id: Option<String>,
) {
    sessions.select_part_chat(&project_id, &part, session_id);
}

/// A pasted image has no path for the attachment list, so it lands in a
/// temporary file first. Each paste gets its own directory so the friendly
/// filename never collides across pastes.
/// A debug build's page reports probe results and console errors here, since a
/// WKWebView has no console a test can read.
#[cfg(debug_assertions)]
#[tauri::command]
fn debug_log(text: String) {
    eprintln!("[page] {text}");
}

#[tauri::command]
fn save_pasted_image(request: tauri::ipc::Request) -> Result<String, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw image bytes".into());
    };
    let mime = request.headers().get("mime").and_then(|m| m.to_str().ok());
    Ok(write_pasted_image(mime, bytes)?
        .to_string_lossy()
        .into_owned())
}

pub(crate) const PASTED_IMAGE: &str = "pasted-image";

fn write_pasted_image(mime: Option<&str>, bytes: &[u8]) -> Result<PathBuf, String> {
    // Keep the real type: the attachment path converts or links by extension,
    // and renaming TIFF or HEIC bytes to PNG makes an invalid embedded image.
    let extension = match mime {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        Some("image/tiff") => "tiff",
        Some("image/bmp") => "bmp",
        Some("image/avif") => "avif",
        Some("image/heic") => "heic",
        Some("image/heif") => "heif",
        Some("image/svg+xml") => "svg",
        Some("image/x-icon") | Some("image/vnd.microsoft.icon") => "ico",
        Some(other) => return Err(format!("cannot paste {other} images")),
        None => return Err("pasted image has no type".into()),
    };
    let dir = std::env::temp_dir().join(format!(
        "nurb-paste-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not save pasted image: {e}"))?;
    let path = dir.join(format!("{PASTED_IMAGE}.{extension}"));
    std::fs::write(&path, bytes).map_err(|e| format!("could not save pasted image: {e}"))?;
    Ok(path)
}

/// Dev-build test hook: this machine's UI automation cannot type into a
/// WKWebView (AX rejects value writes on its text areas), so debug builds
/// accept composer text on a loopback socket and forward it to the webview.
/// Never compiled into release builds.
#[cfg(debug_assertions)]
fn test_hook(app: AppHandle) {
    use std::io::Read;
    std::thread::spawn(move || {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 7399)) else {
            return;
        };
        for stream in listener.incoming().flatten() {
            let mut text = String::new();
            let mut stream = stream;
            if stream.read_to_string(&mut text).is_ok() && !text.is_empty() {
                use tauri::Emitter;
                // `nc` sends the line's newline; a name stored with it is not a
                // part name and every checkout of that project refuses it.
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                // "create:<name>" drives project creation, "open:<name>"
                // switches to a listed project, "send:" submits the visible
                // composer; anything else is composer text. AX presses
                // buttons only while the webview is frontmost and cannot
                // reach WKWebView text fields or list rows, hence all four.
                let _ = if let Some(name) = text.strip_prefix("create:") {
                    app.emit("test-create", name.to_string())
                } else if let Some(name) = text.strip_prefix("open:") {
                    app.emit("test-open", name.to_string())
                } else if let Some(path) = text.strip_prefix("import:") {
                    app.emit("test-import", path.to_string())
                } else if let Some(link) = text.strip_prefix("link:") {
                    // The path a click on "Open in nurb" takes, minus LaunchServices.
                    opened_links(&app, vec![link.to_string()]);
                    Ok(())
                } else if let Some(name) = text.strip_prefix("part:") {
                    app.emit("test-part", name.to_string())
                } else if let Some(js) = text.strip_prefix("eval:") {
                    // Evaluated by the webview itself, so it answers even when the
                    // page's own script never ran; the result comes back through
                    // `debug_log`.
                    use tauri::Manager;
                    let wrapped = format!(
                        // A probe that returns nothing still has to answer with
                        // a string: `undefined` makes the invoke reject, and the
                        // rejection lands in the very error log this reports to.
                        "(async () => {{ let text; try {{ const v = await ({js}); text = typeof v === 'string' ? v : JSON.stringify(v) ?? String(v); }} catch (e) {{ text = 'error: ' + String(e); }} window.__TAURI_INTERNALS__.invoke('debug_log', {{ text }}).catch(() => {{}}); }})()"
                    );
                    match app.get_webview_window("main") {
                        Some(window) => window.eval(&wrapped).map_err(Into::into),
                        None => Ok(()),
                    }
                } else if let Some(assignment) = text.strip_prefix("param:") {
                    // "param:<name>=<value>" moves a slider through the range
                    // input's own events, the same path a drag takes.
                    app.emit("test-param", assignment.to_string())
                } else if text == "send:" {
                    app.emit("test-send", ())
                } else {
                    app.emit("test-type", text)
                };
            }
        }
    });
}

/// macOS ships an empty Help submenu, and a chromeless window has nowhere else to
/// put a link. The two Help items are the only place in the app that reaches the
/// outside world, alongside the same pair in the about box. "Check for Updates…"
/// sits under About where every Mac app keeps it; the webview owns the update
/// state, so the click is forwarded there as an event.
///
/// The app's keyboard shortcuts live here too, as menu items, so the menu bar is
/// where a person finds them. Each one is forwarded to the webview, which owns
/// the screens, as `menu:<action>`.
fn install_menu(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, HELP_SUBMENU_ID};
    let menu = Menu::default(app)?;
    let item = |id: &str, text: &str, key: &str| MenuItem::with_id(app, id, text, true, Some(key));
    let submenus: Vec<_> = menu.items()?.iter().filter_map(|i| i.as_submenu().cloned()).collect();
    let named = |name: &str| submenus.iter().find(|s| s.text().is_ok_and(|t| t == name)).cloned();
    if let Some(appmenu) = submenus.first() {
        appmenu.insert_items(
            &[
                &MenuItem::with_id(app, "app:check-updates", "Check for Updates…", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &item("shortcut:settings", "Settings…", "CmdOrCtrl+,")?,
            ],
            1,
        )?;
    }
    if let Some(file) = named("File") {
        file.insert_items(
            &[
                &item("shortcut:new-part", "New Part", "CmdOrCtrl+N")?,
                &item("shortcut:import-folder", "Import Folder…", "CmdOrCtrl+O")?,
                &PredefinedMenuItem::separator(app)?,
            ],
            0,
        )?;
    }
    if let Some(edit) = named("Edit") {
        edit.append_items(&[
            &PredefinedMenuItem::separator(app)?,
            &item("shortcut:find", "Find Parts", "CmdOrCtrl+F")?,
        ])?;
    }
    if let Some(view) = named("View") {
        view.insert_items(
            &[
                &item("shortcut:library", "Back to Library", "CmdOrCtrl+[")?,
                &PredefinedMenuItem::separator(app)?,
            ],
            0,
        )?;
    }
    if let Some(help) = menu.get(HELP_SUBMENU_ID).and_then(|i| i.as_submenu().cloned()) {
        // Windows' default Help menu carries a predefined About, which is a
        // system message box with its own chime. The app's About is the
        // Settings section the page already draws, so the item opens that.
        #[cfg(windows)]
        {
            for item in help.items()? {
                if item.as_predefined_menuitem().is_some() {
                    help.remove(&item)?;
                }
            }
            help.append_items(&[
                &MenuItem::with_id(app, "shortcut:about", "About nurb", true, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
            ])?;
        }
        help.append_items(&[
            &MenuItem::with_id(app, "help:github", "nurb on GitHub", true, None::<&str>)?,
            &MenuItem::with_id(app, "help:issue", "Report an Issue", true, None::<&str>)?,
        ])?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        use tauri::Emitter;
        use tauri_plugin_opener::OpenerExt;
        let id = event.id().as_ref();
        if let Some(action) = id.strip_prefix("shortcut:") {
            let _ = app.emit(&format!("menu:{action}"), ());
            return;
        }
        let url = match id {
            "app:check-updates" => {
                let _ = app.emit("menu:check-updates", ());
                return;
            }
            "help:github" => "https://github.com/Shpigford/nurb",
            "help:issue" => "https://github.com/Shpigford/nurb/issues/new/choose",
            _ => return,
        };
        let _ = app.opener().open_url(url, None::<&str>);
    });
    Ok(())
}

/// On Windows nurb.exe is also the agent sandbox's launcher (the part
/// sandbox-exec plays on macOS): Some(exit code) when this process was started
/// as that, before any of the app loads.
#[cfg(windows)]
pub fn sandbox_exec() -> Option<i32> {
    acp::sandbox::exec_from_args()
}

/// Size, position and maximized only: restoring visibility or decorations
/// would fight the overlay title bar, and on Windows would show the window
/// before its page loads, which is what `Reveal` waits for.
fn window_state_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let builder = tauri_plugin_window_state::Builder::new().with_state_flags(
        tauri_plugin_window_state::StateFlags::SIZE
            | tauri_plugin_window_state::StateFlags::POSITION
            | tauri_plugin_window_state::StateFlags::MAXIMIZED,
    );
    // A scratch data dir keeps its own window position too, so a test run
    // neither moves the developer's nurb window nor is moved by it. Never
    // compiled into release builds.
    #[cfg(debug_assertions)]
    let builder = match std::env::var_os("NURB_DESKTOP_DATA") {
        Some(dir) => builder.with_filename(
            PathBuf::from(dir)
                .join(".window-state.json")
                .to_string_lossy()
                .into_owned(),
        ),
        None => builder,
    };
    builder.build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(window_state_plugin())
        // Managed before the window exists, so a page that finishes loading
        // before setup runs still finds it armed.
        .manage(Reveal::default())
        .on_page_load(|webview, payload| {
            if webview.label() == "main" && payload.event() == PageLoadEvent::Finished {
                reveal(webview.app_handle());
            }
        })
        .manage(acp::Chats::new())
        .manage(claude::Drivers::new())
        .manage(agents::Logins::new())
        .manage(provision::Provisioner::new())
        .manage(OpenedLinks::default())
        .setup(|app| {
            // The login shell can take a second; ask it before the rail's
            // first agent check needs the answer.
            std::thread::spawn(|| {
                env::user_path();
            });
            // Windows splits app data into Roaming and Local; a ~1 GB Python
            // and Node environment belongs in Local, never in a profile that
            // syncs between machines. On macOS both are Application Support.
            #[cfg(windows)]
            let dir = app.path().app_local_data_dir()?;
            #[cfg(not(windows))]
            let dir = app.path().app_data_dir()?;
            // Debug-only override so tests can point the whole app (sessions,
            // projects, provisioned env, the serve's home) at a scratch
            // directory while HOME stays real. Never compiled into release
            // builds.
            #[cfg(debug_assertions)]
            let dir = std::env::var_os("NURB_DESKTOP_DATA")
                .map(PathBuf::from)
                .unwrap_or(dir);
            std::fs::create_dir_all(&dir)?;
            app.manage(crash::init(&dir, &app.package_info().version.to_string()));
            let launcher = env::Launcher::resolve(dir.clone());
            app.manage(Supervisor::new(
                launcher.clone(),
                dir.clone(),
                app.package_info().version.to_string(),
            ));
            app.manage(launcher);
            app.manage(sessions::SessionStore::load(&dir));
            app.manage(prefs::PrefStore::load(&dir));
            let files = files::Files::load(&dir);
            files::grant(app.handle(), &files);
            app.manage(files);
            app.manage(AppData(dir));
            install_menu(app.handle())?;
            let hidden = app
                .get_webview_window("main")
                .is_some_and(|window| !window.is_visible().unwrap_or(true));
            if hidden {
                let app = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(REVEAL_DEADLINE);
                    reveal(&app);
                });
            } else {
                app.state::<Reveal>().0.store(false, Ordering::SeqCst);
            }
            #[cfg(debug_assertions)]
            test_hook(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            serve_info,
            serve_log,
            project_dir,
            take_v1_folders,
            take_open_links,
            select_part_chat,
            rename_part_chat,
            saved_chat,
            save_pasted_image,
            crash::crash_reports,
            crash::set_crash_reports,
            #[cfg(debug_assertions)]
            debug_log,
            acp::start_chat,
            acp::list_sessions,
            acp::send_prompt,
            acp::check_attachments,
            acp::attachment_thumbnail,
            acp::cancel_turn,
            acp::respond_permission,
            acp::chat_config,
            acp::probe_chat_config,
            acp::set_chat_config,
            acp::close_chat,
            agents::agent_statuses,
            agents::agent_login,
            agents::agent_logout,
            files::storage_locations,
            files::app_data_size,
            files::export_folder,
            files::choose_export_folder,
            files::reset_export_folder,
            files::show_location,
            files::open_in_slicer,
            naming::name_part,
            provision::provision_status,
            provision::parts_import_ok,
            provision::provision,
            provision::about_info
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let RunEvent::Opened { urls } = &event {
                opened_links(app, urls.iter().map(|url| url.to_string()).collect());
            }
            if let RunEvent::Exit = event {
                app.state::<Supervisor>().shutdown();
                app.state::<acp::Chats>().shutdown();
                app.state::<claude::Drivers>().shutdown();
                app.state::<agents::Logins>().shutdown();
                app.state::<provision::Provisioner>().shutdown();
                crash::flush();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{log_tail, write_pasted_image};

    #[test]
    fn log_tail_never_splits_utf8() {
        let text = format!("{}x", "é".repeat(2500));
        let tail = log_tail(&text, 4000);

        assert!(tail.len() <= 4000);
        assert!(tail.ends_with('x'));
        assert!(tail.starts_with('é'));
    }

    #[test]
    fn pasted_images_land_in_unique_files_with_honest_extensions() {
        let png = write_pasted_image(Some("image/png"), b"png bytes").unwrap();
        assert_eq!(png.file_name().unwrap(), "pasted-image.png");
        assert_eq!(std::fs::read(&png).unwrap(), b"png bytes");

        let jpg = write_pasted_image(Some("image/jpeg"), b"jpg bytes").unwrap();
        assert_eq!(jpg.file_name().unwrap(), "pasted-image.jpg");
        // Two pastes in one session must not overwrite each other.
        assert_ne!(png.parent(), jpg.parent());

        // Finder preserves native image bytes, so non-embeddable types must
        // retain an honest extension and travel as file links.
        let odd = write_pasted_image(Some("image/tiff"), b"bytes").unwrap();
        assert_eq!(odd.file_name().unwrap(), "pasted-image.tiff");
        assert!(write_pasted_image(Some("image/vnd.unknown"), b"bytes")
            .unwrap_err()
            .contains("cannot paste"));
        assert!(write_pasted_image(None, b"bytes")
            .unwrap_err()
            .contains("no type"));

        for path in [png, jpg, odd] {
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn transport_errors_still_evict_the_live_chat_session() {
        let failed = Err(agent_client_protocol::Error::internal_error());
        let live = Some("session-1".to_string());

        assert_eq!(
            super::acp::session_to_remove(&failed, &live).as_deref(),
            Some("session-1")
        );
        assert_eq!(super::acp::session_to_remove(&Ok(None), &live), None);
    }
}
