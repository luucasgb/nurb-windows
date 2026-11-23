//! On Windows the app opens one window, once, already where it was left and
//! with its page inside it. Created visible, the window used to sit blank at
//! the system's default spot while WebView2 started, then jump to the saved
//! position, which to anyone watching was a placeholder window closing and
//! the real one opening.
//!
//! This launches the real app against a scratch data dir and watches every
//! top-level window its process owns from outside, the way a person sees them.
#![cfg(windows)]

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

/// Where the scratch window state says the window was left: away from the
/// diagonal Windows cascades new windows along, so a window shown at the
/// default spot can never pass for it.
const SAVED: (i32, i32) = (120, 90);
/// A cold WebView2 on a CI runner can take a while; the app shows its window
/// by itself five seconds after setup even when the page never loads.
const SHOW_DEADLINE: Duration = Duration::from_secs(60);
/// How long the window is watched after it appears, for a late move or resize.
const SETTLE: Duration = Duration::from_secs(3);
/// The console-subsystem debug build would otherwise open a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, PartialEq, Debug)]
struct Seen {
    shown: bool,
    rect: (i32, i32, i32, i32),
    /// The webview's renderer has a window inside this one: there is a page.
    page: bool,
}

/// Ends the app and everything it started, WebView2 included, however the
/// test exits.
struct App(Child);

impl Drop for App {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.0.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.wait();
    }
}

unsafe extern "system" fn collect(hwnd: HWND, found: LPARAM) -> i32 {
    unsafe { (*(found as *mut Vec<HWND>)).push(hwnd) };
    1
}

fn top_level_windows(pid: u32) -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut all as *mut Vec<HWND> as LPARAM) };
    all.into_iter()
        .filter(|&hwnd| {
            let mut owner = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            // Tool windows are the event loop's own invisible helpers, never
            // something a person sees as a window.
            owner == pid
                && unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW == 0
        })
        .collect()
}

fn class(hwnd: HWND) -> String {
    let mut name = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len.max(0) as usize])
}

fn has_page(hwnd: HWND) -> bool {
    let mut children: Vec<HWND> = Vec::new();
    unsafe { EnumChildWindows(hwnd, Some(collect), &mut children as *mut Vec<HWND> as LPARAM) };
    children
        .into_iter()
        .any(|child| class(child) == "Chrome_RenderWidgetHostHWND")
}

fn seen(hwnd: HWND) -> Seen {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(hwnd, &mut rect) };
    Seen {
        shown: unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 },
        rect: (rect.left, rect.top, rect.right, rect.bottom),
        page: has_page(hwnd),
    }
}

fn scratch_data() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nurb-startup-window-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // What the window-state plugin writes when the window closes, read back
    // from the data dir in a debug build.
    let state = serde_json::json!({
        "main": {
            "width": 1300, "height": 760,
            "x": SAVED.0, "y": SAVED.1, "prev_x": SAVED.0, "prev_y": SAVED.1,
            "maximized": false, "visible": true, "decorated": true, "fullscreen": false
        }
    });
    std::fs::write(dir.join(".window-state.json"), state.to_string()).unwrap();
    dir
}

#[test]
fn the_window_appears_once_where_it_was_left_with_its_page() {
    let data = scratch_data();
    let mut app = App(
        Command::new(env!("CARGO_BIN_EXE_nurb"))
            .env("NURB_DESKTOP_DATA", &data)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start the app"),
    );
    let pid = app.0.id();

    let start = Instant::now();
    let mut log: Vec<(Duration, usize, Seen)> = Vec::new();
    let mut last: HashMap<usize, Seen> = HashMap::new();
    let mut appeared: Option<Instant> = None;
    loop {
        let now = Instant::now();
        match appeared {
            Some(at) if now - at > SETTLE => break,
            None if now - start > SHOW_DEADLINE => {
                panic!("no window appeared within {SHOW_DEADLINE:?}; saw {log:#?}")
            }
            _ => {}
        }
        if let Ok(Some(status)) = app.0.try_wait() {
            panic!("the app exited with {status} while being watched; saw {log:#?}");
        }
        for hwnd in top_level_windows(pid) {
            let state = seen(hwnd);
            if last.get(&(hwnd as usize)) != Some(&state) {
                last.insert(hwnd as usize, state);
                log.push((now - start, hwnd as usize, state));
            }
            if state.shown && appeared.is_none() {
                appeared = Some(now);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(app);
    let _ = std::fs::remove_dir_all(&data);

    let history = log
        .iter()
        .map(|(at, hwnd, s)| format!("{:>6}ms {hwnd:#x} {s:?}", at.as_millis()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut shown: Vec<usize> = log.iter().filter(|(_, _, s)| s.shown).map(|(_, h, _)| *h).collect();
    shown.sort_unstable();
    shown.dedup();
    assert_eq!(shown.len(), 1, "expected exactly one window to appear:\n{history}");

    let after: Vec<Seen> = log
        .iter()
        .filter(|(_, hwnd, _)| *hwnd == shown[0])
        .map(|(_, _, s)| *s)
        .skip_while(|s| !s.shown)
        .collect();
    let first = after[0];
    assert_eq!(
        (first.rect.0, first.rect.1),
        SAVED,
        "the window first appeared somewhere other than where it was left:\n{history}"
    );
    assert!(first.page, "the window appeared before its page was inside it:\n{history}");
    assert!(
        after.iter().all(|s| s.shown && s.rect == first.rect),
        "the window moved, resized or hid after it appeared:\n{history}"
    );
}
