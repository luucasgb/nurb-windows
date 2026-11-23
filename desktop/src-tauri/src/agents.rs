//! The agents the app can host, and everything that differs between them:
//! how their ACP process starts, display name, sign-in state, and the login
//! flow.
//!
//! Codex runs through an npm adapter the app provisions, and that adapter
//! bundles its agent's real CLI (codex-acp carries @openai/codex). Gemini's
//! official CLI speaks ACP itself and is provisioned beside it, so nothing has
//! to be on the user's PATH except node/npx. Cursor and Grok ship CLIs that speak ACP natively, and Claude's CLI is driven directly in stream-json (see claude.rs), so the app never installs those three: it finds the binary the
//! vendor's own installer put on the machine. Signing in through the app
//! shares credentials with any terminal install either way, because every
//! agent reads its own store (~/.claude, ~/.codex, Cursor's, ~/.grok).

#[cfg(not(windows))]
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine as _;
use serde_json::Value;

use crate::platform::NoWindow;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    /// The user's own Claude Code CLI, driven directly (see claude.rs).
    Claude,
    Codex,
    Gemini,
    Cursor,
    Grok,
}

pub const ALL: [AgentKind; 5] = [
    AgentKind::Claude,
    AgentKind::Codex,
    AgentKind::Gemini,
    AgentKind::Cursor,
    AgentKind::Grok,
];

impl AgentKind {
    pub fn parse(id: &str) -> Result<Self, String> {
        match id {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            "cursor" => Ok(Self::Cursor),
            "grok" => Ok(Self::Grok),
            other => Err(format!("unknown agent: {other}")),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
        }
    }

    /// The name the UI shows. "Claude", not "Claude Code": the audience is
    /// hobbyists, and the Code suffix is developer branding.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
            Self::Cursor => "Cursor",
            Self::Grok => "Grok",
        }
    }

    /// Exact adapter pins for the adapter-hosted agents; both projects
    /// release weekly, so no ranges. None for the ACP-native CLIs.
    pub fn adapter(self) -> Option<&'static str> {
        match self {
            Self::Codex => Some("@agentclientprotocol/codex-acp@2.5.0"),
            Self::Gemini => Some("@google/gemini-cli@0.66.0"),
            Self::Claude | Self::Cursor | Self::Grok => None,
        }
    }

    /// The executable name npm installs for the adapter, for spawning it out
    /// of the provisioned runtime without npx.
    pub fn adapter_bin(self) -> Option<&'static str> {
        match self {
            Self::Codex => Some("codex-acp"),
            Self::Gemini => Some("gemini"),
            Self::Claude | Self::Cursor | Self::Grok => None,
        }
    }

    /// Binary name and the args that start ACP over stdio, for the CLIs that
    /// speak it natively. None for the adapter-hosted agents.
    pub fn native_command(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            // The driver builds its own argv, so the pair carries the binary
            // name and nothing else.
            Self::Claude => Some(("claude", &[] as &[&str])),
            // `cursor-agent`, not `agent`: Cursor's installer links both, but
            // Grok's installer also claims `agent`, so only the long name is
            // sure to be Cursor's.
            Self::Cursor => Some(("cursor-agent", &["acp"])),
            Self::Grok => Some(("grok", &["agent", "stdio"])),
            Self::Codex | Self::Gemini => None,
        }
    }

    /// Where a native CLI actually is: the vendor installer's fixed spot
    /// first, then the user's shell PATH for Homebrew, npm and other installs. None when it is not on this
    /// machine, which is what "installed: false" means for these agents.
    pub fn native_bin(self) -> Option<PathBuf> {
        let (name, _) = self.native_command()?;
        // Claude's own installer writes one of two spots depending on its age.
        let install_dirs: &[&str] = match self {
            Self::Claude => &[".local/bin", ".claude/local"],
            Self::Cursor => &[".local/bin"],
            Self::Grok => &[".grok/bin"],
            Self::Codex | Self::Gemini => return None,
        };
        let home = crate::platform::home()?;
        find_bin(name, &home, install_dirs, Some(crate::env::user_path().into()))
    }

    /// What a signed-out user needs, in the pane and the chat column.
    pub fn subscription_note(self) -> &'static str {
        match self {
            Self::Claude => "works with a Claude subscription (Pro, from $20/month)",
            Self::Codex => "works with a ChatGPT subscription (Go, from $8/month)",
            Self::Gemini => "works with a Gemini API key from Google AI Studio",
            Self::Cursor => "works with a Cursor subscription (Pro, from $20/month)",
            Self::Grok => "works with an xAI subscription (SuperGrok, from $30/month)",
        }
    }

    /// Whether the app can sign this agent out, and so switch its account.
    /// Grok's CLI has no sign-out the app has been able to check.
    pub fn can_sign_out(self) -> bool {
        !matches!(self, Self::Grok)
    }

    /// The vendor's one-line installer, for the "need another agent?" help.
    /// Only the native CLIs have one; the adapter-hosted pair arrive with the
    /// app and are never absent outside a broken dev machine.
    pub fn install_command(self) -> Option<&'static str> {
        #[cfg(windows)]
        return match self {
            // PowerShell, the vendors' own Windows one-liners.
            Self::Claude => Some("irm https://claude.ai/install.ps1 | iex"),
            Self::Cursor => Some("irm 'https://cursor.com/install?win32=true' | iex"),
            Self::Grok | Self::Codex | Self::Gemini => None,
        };
        #[cfg(not(windows))]
        match self {
            Self::Claude => Some("curl -fsSL https://claude.ai/install.sh | bash"),
            Self::Cursor => Some("curl https://cursor.com/install -fsSL | bash"),
            Self::Grok => Some("curl -fsSL https://x.ai/cli/install.sh | bash"),
            Self::Codex | Self::Gemini => None,
        }
    }
}

fn find_bin(
    name: &str,
    home: &std::path::Path,
    install_dirs: &[&str],
    path: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    // `claude.exe` on Windows, where only an .exe can be started directly.
    let name = crate::platform::exe(name);
    for dir in install_dirs {
        let default = home.join(dir).join(&name);
        if default.is_file() {
            return Some(default);
        }
    }
    std::env::split_paths(&path?)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    id: &'static str,
    label: &'static str,
    /// Whether the adapter can run at all: npx on PATH in dev, the
    /// provisioned node and adapter install otherwise.
    installed: bool,
    /// None means the check itself failed, not signed out.
    logged_in: Option<bool>,
    /// Who it is signed in as (an email, or which key), when the store says.
    account: Option<String>,
    /// Extra detail worth showing ("Max plan"), when the check provides it.
    detail: Option<String>,
    can_sign_out: bool,
    note: &'static str,
    /// The vendor's installer command, for agents the app finds but the user
    /// installs. The rail hides uninstalled agents; the help modal uses this.
    install: Option<&'static str>,
}

#[tauri::command]
pub async fn agent_statuses(app: tauri::AppHandle) -> Vec<AgentStatus> {
    use tauri::Manager;
    let launcher = app.state::<crate::env::Launcher>().inner().clone();
    let checks = ALL.map(|agent| {
        let launcher = launcher.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let installed = launcher.adapter_available(agent);
            let login = if installed {
                match agent {
                    AgentKind::Claude => driver_auth_status(),
                    AgentKind::Codex => codex_login(),
                    AgentKind::Gemini => match gemini_api_key() {
                        Ok(key) => Login {
                            signed_in: Some(true),
                            account: Some(key_hint(&key)),
                            detail: None,
                        },
                        Err(_) => Login::known(false),
                    },
                    AgentKind::Cursor => cursor_auth_status(agent),
                    AgentKind::Grok => Login::known(auth_file(".grok").is_file()),
                }
            } else {
                Login::unknown()
            };
            AgentStatus {
                id: agent.id(),
                label: agent.label(),
                installed,
                logged_in: login.signed_in,
                account: login.account,
                detail: login.detail,
                can_sign_out: agent.can_sign_out(),
                note: agent.subscription_note(),
                install: agent.install_command(),
            }
        })
    });
    let mut statuses = Vec::new();
    for (agent, check) in ALL.into_iter().zip(checks) {
        statuses.push(check.await.unwrap_or(AgentStatus {
            id: agent.id(),
            label: agent.label(),
            installed: false,
            logged_in: None,
            account: None,
            detail: None,
            can_sign_out: agent.can_sign_out(),
            note: agent.subscription_note(),
            install: agent.install_command(),
        }));
    }
    statuses
}

/// What a status check found. `signed_in: None` means the check itself
/// failed, not signed out.
struct Login {
    signed_in: Option<bool>,
    account: Option<String>,
    detail: Option<String>,
}

impl Login {
    fn known(signed_in: bool) -> Self {
        Self {
            signed_in: Some(signed_in),
            account: None,
            detail: None,
        }
    }

    fn unknown() -> Self {
        Self {
            signed_in: None,
            account: None,
            detail: None,
        }
    }
}

/// "max" as the row shows it: "Max plan".
fn plan_label(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(first) => format!("{}{} plan", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// The driver's own check: `claude auth status` exits 0 signed in and
/// non-zero signed out, and prints JSON with `--json` on the versions that
/// have it, which also carries the account and the plan.
fn driver_auth_status() -> Login {
    let Some(bin) = AgentKind::Claude.native_bin() else {
        return Login::unknown();
    };
    let Ok(output) = Command::new(bin)
        .args(["auth", "status", "--json"])
        .env("PATH", crate::env::user_path())
        .no_window()
        .output() else {
        return Login::unknown();
    };
    let parsed = serde_json::from_slice::<Value>(&output.stdout).ok();
    claude_login(parsed.as_ref(), output.status.success())
}

fn claude_login(json: Option<&Value>, exited_ok: bool) -> Login {
    let field = |name: &str| {
        json.and_then(|json| json.get(name))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let signed_in = json
        .and_then(|json| json.get("loggedIn"))
        .and_then(Value::as_bool)
        .unwrap_or(exited_ok);
    if !signed_in {
        return Login::known(false);
    }
    Login {
        signed_in: Some(true),
        account: field("email").map(str::to_string),
        detail: field("subscriptionType").map(plan_label),
    }
}

fn codex_login() -> Login {
    let file = auth_file(".codex");
    if !file.is_file() {
        return Login::known(false);
    }
    let (account, detail) = std::fs::read_to_string(&file)
        .map(|text| codex_account(&text))
        .unwrap_or_default();
    Login {
        signed_in: Some(true),
        account,
        detail,
    }
}

/// Who Codex's auth.json is signed in as, without its secrets: the email and
/// plan from the ChatGPT sign-in's id token, or that it is an API key.
fn codex_account(text: &str) -> (Option<String>, Option<String>) {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return (None, None);
    };
    if json.get("auth_mode").and_then(Value::as_str) == Some("apikey") {
        return (Some("API key".into()), None);
    }
    let Some(claims) = json
        .pointer("/tokens/id_token")
        .and_then(Value::as_str)
        .and_then(jwt_claims)
    else {
        let api_key = json
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.is_empty());
        return (api_key.then(|| "API key".into()), None);
    };
    let email = claims.get("email").and_then(Value::as_str).map(str::to_string);
    let plan = claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .filter(|plan| !plan.is_empty())
        .map(plan_label);
    (email, plan)
}

/// A JWT's payload, read and never verified: it only labels a row.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Enough of a key to tell two apart, never enough to use: the last four
/// characters of a real key, nothing of one too short to spare them.
fn key_hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 16 {
        return "API key".into();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("Key ending {tail}")
}

/// The first thing in a CLI's status text that reads as an email address.
fn email_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '"' | '\'' | ','))
        .map(|word| word.trim_end_matches(['.', ':', ';']))
        .find(|word| {
            word.split_once('@').is_some_and(|(name, domain)| {
                !name.is_empty() && domain.contains('.') && !domain.starts_with('.')
            })
        })
        .map(str::to_string)
}

/// Codex and Grok keep no status subcommand worth trusting, but both write
/// `auth.json` on every login and delete it on logout, so existence is the
/// signal. (An expired token still reads as signed in; the chat's -32000
/// handling catches that honestly on first use.) Codex alone honors a HOME
/// override env var.
fn auth_file(dir: &str) -> PathBuf {
    let home = if dir == ".codex" {
        std::env::var("CODEX_HOME").map(PathBuf::from).ok()
    } else {
        None
    };
    home.unwrap_or_else(|| crate::platform::home().unwrap_or_default().join(dir))
        .join("auth.json")
}

const GEMINI_KEYCHAIN_SERVICE: &str = "dev.nurb.desktop.gemini-api-key";
const GEMINI_KEYCHAIN_ACCOUNT: &str = "gemini";

/// The key's name in the Keychain or Credential Manager. A debug run against a
/// scratch data dir keeps a key of its own, so trying Replace or Remove there
/// never touches the developer's real one. Never compiled into release builds.
fn gemini_service() -> String {
    #[cfg(debug_assertions)]
    if std::env::var_os("NURB_DESKTOP_DATA").is_some() {
        return format!("{GEMINI_KEYCHAIN_SERVICE}.scratch");
    }
    GEMINI_KEYCHAIN_SERVICE.to_string()
}

#[cfg(not(windows))]
pub(crate) fn gemini_api_key() -> Result<String, String> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            GEMINI_KEYCHAIN_ACCOUNT,
            "-s",
            &gemini_service(),
            "-w",
        ])
        .output()
        .map_err(|error| format!("could not read the Gemini API key: {error}"))?;
    if !output.status.success() {
        return Err("Gemini API key not found".into());
    }
    let key = String::from_utf8(output.stdout)
        .map_err(|_| "the Gemini API key is not valid text".to_string())?
        .trim()
        .to_string();
    if key.is_empty() {
        Err("Gemini API key is empty".into())
    } else {
        Ok(key)
    }
}

#[cfg(not(windows))]
fn save_gemini_api_key(key: &str) -> Result<(), String> {
    let key = security_interactive_argument(key)?;
    let service = gemini_service();
    let command = format!(
        "add-generic-password -a \"{GEMINI_KEYCHAIN_ACCOUNT}\" -s \"{service}\" -w {key} -U\n"
    );
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not save the Gemini API key: {error}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "could not open macOS Keychain input".to_string())?
        .write_all(command.as_bytes());
    let status = child
        .wait()
        .map_err(|error| format!("could not save the Gemini API key: {error}"))?;
    write_result.map_err(|error| format!("could not save the Gemini API key: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "macOS Keychain did not save the Gemini API key".into())
}

#[cfg(not(windows))]
fn delete_gemini_api_key() -> Result<(), String> {
    let status = Command::new("/usr/bin/security")
        .args([
            "delete-generic-password",
            "-a",
            GEMINI_KEYCHAIN_ACCOUNT,
            "-s",
            &gemini_service(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("could not remove the Gemini API key: {error}"))?;
    // 44 is errSecItemNotFound: already gone, which is what was asked.
    if status.success() || status.code() == Some(44) {
        Ok(())
    } else {
        Err("macOS Keychain did not remove the Gemini API key".into())
    }
}

#[cfg(not(windows))]
fn security_interactive_argument(value: &str) -> Result<String, String> {
    if value.contains(['\r', '\n']) {
        return Err("Gemini API key contains an invalid line break".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Windows keeps the key in Credential Manager, its Keychain: a generic
/// credential under the same service name, readable only by this user.
#[cfg(windows)]
pub(crate) fn gemini_api_key() -> Result<String, String> {
    use crate::platform::windows::wide;
    use windows_sys::Win32::Security::Credentials::{CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC};
    let target = wide(std::ffi::OsStr::new(&gemini_service()));
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
        return Err("Gemini API key not found".into());
    }
    let bytes = unsafe {
        let blob = &*credential;
        let bytes = if blob.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(blob.CredentialBlob, blob.CredentialBlobSize as usize).to_vec()
        };
        CredFree(credential.cast());
        bytes
    };
    let key = String::from_utf8(bytes)
        .map_err(|_| "the Gemini API key is not valid text".to_string())?
        .trim()
        .to_string();
    if key.is_empty() {
        Err("Gemini API key is empty".into())
    } else {
        Ok(key)
    }
}

#[cfg(windows)]
fn save_gemini_api_key(key: &str) -> Result<(), String> {
    use crate::platform::windows::wide;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };
    let mut target = wide(std::ffi::OsStr::new(&gemini_service()));
    let mut account = wide(std::ffi::OsStr::new(GEMINI_KEYCHAIN_ACCOUNT));
    let blob = key.as_bytes();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: account.as_mut_ptr(),
    };
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        return Err(format!(
            "Windows Credential Manager did not save the Gemini API key: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn delete_gemini_api_key() -> Result<(), String> {
    use crate::platform::windows::wide;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
    let target = wide(std::ffi::OsStr::new(&gemini_service()));
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        // Already gone, which is what was asked.
        if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
            return Err(format!(
                "Windows Credential Manager did not remove the Gemini API key: {error}"
            ));
        }
    }
    Ok(())
}

/// `cursor-agent status` prints "Not logged in" signed out and account details
/// signed in, with no JSON form, so the text is the signal and anything
/// unrecognizable is honestly unknown rather than guessed.
fn cursor_auth_status(kind: AgentKind) -> Login {
    let Some(bin) = kind.native_bin() else {
        return Login::unknown();
    };
    let Ok(output) = Command::new(bin)
        .arg("status")
        .env("PATH", crate::env::user_path())
        .no_window()
        .output() else {
        return Login::unknown();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    if text.contains("Not logged in") {
        Login::known(false)
    } else if output.status.success() && !text.trim().is_empty() {
        Login {
            signed_in: Some(true),
            account: email_in(&text),
            detail: None,
        }
    } else {
        Login::unknown()
    }
}

/// Login children still running at app exit, killed by process group like
/// every other child the app spawns.
pub struct Logins(Mutex<Vec<i32>>);

impl Logins {
    pub fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    pub fn shutdown(&self) {
        for pgid in self.0.lock().unwrap().drain(..) {
            crate::platform::killpg(pgid, crate::platform::SIGTERM);
        }
    }
}

/// Every agent ships a browser OAuth flow as a CLI subcommand that opens the
/// browser itself and exits 0 once the login lands in the shared credential
/// store. Driving those beats holding an ACP `authenticate` request pending
/// for however long a human takes in a browser.
#[tauri::command]
pub async fn agent_login(
    app: tauri::AppHandle,
    agent: String,
    api_key: Option<String>,
) -> Result<(), String> {
    use tauri::Manager;
    let kind = AgentKind::parse(&agent)?;
    if kind == AgentKind::Gemini {
        let key = api_key
            .filter(|key| !key.trim().is_empty())
            .ok_or("Enter a Gemini API key from Google AI Studio.")?;
        let replacing = gemini_api_key().is_ok();
        crate::acp::authenticate(app.clone(), kind, "gemini-api-key", Some(&key)).await?;
        save_gemini_api_key(&key)?;
        // A running Gemini was handed the old key when it started.
        if replacing {
            end_chats(&app, kind, "Gemini has a new key, so this chat ended. Send a message to start a fresh one.");
        }
        return Ok(());
    }
    let launcher = app.state::<crate::env::Launcher>();
    let (program, mut args) = launcher.adapter(kind);
    let adapter_path = launcher.adapter_path();
    // codex-acp's login spawns `codex app-server` off PATH, unlike the rest of
    // the adapter, which falls back to the copy npm installed beside it. On a
    // user's machine nothing but node is on that PATH, so the spawn fails with
    // ENOENT and the login reports only its exit code. Point it at the
    // bundled CLI. (Dev checkouts run the adapter through npx, which puts that
    // same CLI on PATH itself, which is why this only ever broke the shipped
    // app.)
    let codex_cli = match kind {
        AgentKind::Codex => launcher.paths().map(crate::env::Paths::codex_exe),
        _ => None,
    };
    match kind {
        AgentKind::Codex => args.push("login".into()),
        // The driver still never touches credentials: this is Claude's own
        // sign-in, which opens the browser itself and waits for it without a
        // terminal, writing the same store the user's terminal reads.
        AgentKind::Claude => args = vec!["auth".into(), "login".into()],
        // Native CLIs: drop the ACP args the launcher put on, login is its
        // own subcommand.
        AgentKind::Cursor | AgentKind::Grok => args = vec!["login".into()],
        AgentKind::Gemini => unreachable!(),
    }
    let (pgid_tx, pgid_rx) = std::sync::mpsc::channel::<i32>();
    let done = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        use crate::platform::OwnGroup;
        let mut command = Command::new(program);
        if let Some(path) = adapter_path {
            command.env("PATH", path);
        }
        if let Some(cli) = codex_cli {
            command.env("CODEX_PATH", cli);
        }
        let mut child = command
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .own_group()
            .spawn()
            .map_err(|e| format!("could not start the sign-in: {e}"))?;
        let pgid = child.id() as i32;
        let _ = pgid_tx.send(pgid);
        let handle = app.state::<Logins>();
        handle.0.lock().unwrap().push(pgid);
        let status = child.wait();
        handle.0.lock().unwrap().retain(|p| *p != pgid);
        let status = status.map_err(|e| format!("sign-in failed: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            // No terminal advice: the audience is hobbyists, and the usual
            // cause is a browser tab closed before the login landed.
            Err("The sign-in did not finish. Try again, and complete the sign-in in the browser tab that opens.".into())
        }
    });
    // A human in a browser sets the pace; ten minutes is generous. On timeout
    // the process group is killed, which also unblocks the waiting thread.
    match tokio::time::timeout(Duration::from_secs(600), done).await {
        Ok(joined) => joined.map_err(|e| e.to_string())?,
        Err(_) => {
            if let Ok(pgid) = pgid_rx.try_recv() {
                crate::platform::killpg(pgid, crate::platform::SIGTERM);
            }
            Err("The sign-in timed out. Try again.".into())
        }
    }
}

/// Signs an agent out through its own CLI, so its store and any terminal
/// install agree: the sign-in is shared, and so is the sign-out. Gemini's key
/// is the app's own, so it is removed here.
#[tauri::command]
pub async fn agent_logout(app: tauri::AppHandle, agent: String) -> Result<(), String> {
    use tauri::Manager;
    let kind = AgentKind::parse(&agent)?;
    if kind == AgentKind::Gemini {
        delete_gemini_api_key()?;
    } else {
        let launcher = app.state::<crate::env::Launcher>().inner().clone();
        tauri::async_runtime::spawn_blocking(move || {
            let output = logout_command(&launcher, kind)?
                .stdin(Stdio::null())
                .no_window()
                .output()
                .map_err(|e| format!("could not start the sign-out: {e}"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(format!("{} did not sign out. Try again.", kind.label()))
            }
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    end_chats(
        &app,
        kind,
        &format!("{} was signed out, so this chat ended. Send a message to start a fresh one.", kind.label()),
    );
    Ok(())
}

/// Ends the agent's open chats, which keep the sign-in or key they started
/// with until their process does.
fn end_chats(app: &tauri::AppHandle, kind: AgentKind, why: &str) {
    use tauri::Manager;
    if kind == AgentKind::Claude {
        app.state::<crate::claude::Drivers>().end_all();
    } else {
        app.state::<crate::acp::Chats>().end_agent(kind, why);
    }
}

fn logout_command(launcher: &crate::env::Launcher, kind: AgentKind) -> Result<Command, String> {
    match kind {
        AgentKind::Claude | AgentKind::Cursor => {
            let bin = kind
                .native_bin()
                .ok_or_else(|| format!("{} is not on this computer.", kind.label()))?;
            let mut command = Command::new(bin);
            if kind == AgentKind::Claude {
                command.args(["auth", "logout"]);
            } else {
                command.arg("logout");
            }
            command.env("PATH", crate::env::user_path());
            Ok(command)
        }
        AgentKind::Codex => match launcher.paths() {
            Some(paths) => {
                let mut command = Command::new(paths.codex_exe());
                command.arg("logout");
                if let Some(path) = launcher.adapter_path() {
                    command.env("PATH", path);
                }
                Ok(command)
            }
            // A checkout runs the adapter through npx, which carries the same
            // CLI the provisioned app bundles.
            None => {
                let mut command = Command::new(if cfg!(windows) { "npx.cmd" } else { "npx" });
                command.args(["-y", "-p", kind.adapter().expect("adapter-hosted"), "codex", "logout"]);
                Ok(command)
            }
        },
        AgentKind::Grok => Err("Sign Grok out from its own app.".into()),
        AgentKind::Gemini => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::{claude_login, codex_account, email_in, key_hint, plan_label, AgentKind};
    use base64::Engine as _;

    fn id_token(claims: serde_json::Value) -> String {
        let encode = |value: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!("{}.{}.signature", encode(&serde_json::json!({"alg": "RS256"})), encode(&claims))
    }

    #[test]
    fn a_chatgpt_sign_in_names_its_email_and_plan() {
        let auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {"id_token": id_token(serde_json::json!({
                "email": "maker@example.com",
                "https://api.openai.com/auth": {"chatgpt_plan_type": "plus"}
            }))}
        });
        assert_eq!(
            codex_account(&auth.to_string()),
            (Some("maker@example.com".into()), Some("Plus plan".into()))
        );
    }

    #[test]
    fn a_codex_api_key_is_named_as_one_and_never_shown() {
        let auth = serde_json::json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-secret"});
        assert_eq!(codex_account(&auth.to_string()), (Some("API key".into()), None));
        let older = serde_json::json!({"OPENAI_API_KEY": "sk-secret", "tokens": null});
        assert_eq!(codex_account(&older.to_string()), (Some("API key".into()), None));
    }

    #[test]
    fn an_unreadable_codex_store_still_reads_as_signed_in_without_a_name() {
        assert_eq!(codex_account("not json"), (None, None));
        let broken = serde_json::json!({"auth_mode": "chatgpt", "tokens": {"id_token": "a.%%%.c"}});
        assert_eq!(codex_account(&broken.to_string()), (None, None));
    }

    #[test]
    fn claude_status_gives_the_account_and_plan_only_when_signed_in() {
        let signed_in = serde_json::json!({
            "loggedIn": true, "email": "maker@example.com", "subscriptionType": "max"
        });
        let login = claude_login(Some(&signed_in), true);
        assert_eq!(login.signed_in, Some(true));
        assert_eq!(login.account.as_deref(), Some("maker@example.com"));
        assert_eq!(login.detail.as_deref(), Some("Max plan"));

        let signed_out = serde_json::json!({"loggedIn": false, "email": "maker@example.com"});
        let login = claude_login(Some(&signed_out), false);
        assert_eq!(login.signed_in, Some(false));
        assert_eq!(login.account, None);

        // Versions without --json answer by exit code alone.
        assert_eq!(claude_login(None, true).signed_in, Some(true));
    }

    #[test]
    fn a_key_hint_shows_four_characters_of_a_real_key_and_none_of_a_short_one() {
        assert_eq!(key_hint("AIzaSyD-0123456789abcdefghijklmnopqrWXYZ"), "Key ending WXYZ");
        assert_eq!(key_hint("short-key"), "API key");
    }

    #[test]
    fn an_email_is_found_in_free_status_text() {
        assert_eq!(
            email_in("\u{2713} Logged in as maker@example.com.").as_deref(),
            Some("maker@example.com")
        );
        assert_eq!(email_in("Account: <maker@example.co.uk>").as_deref(), Some("maker@example.co.uk"));
        assert_eq!(email_in("Logged in"), None);
        assert_eq!(email_in("user@localhost"), None);
    }

    #[test]
    fn a_plan_reads_as_a_capitalised_plan() {
        assert_eq!(plan_label("team"), "Team plan");
        assert_eq!(plan_label(""), "");
    }

    #[test]
    fn only_grok_cannot_be_signed_out_from_the_app() {
        for kind in super::ALL {
            assert_eq!(kind.can_sign_out(), kind != AgentKind::Grok, "{kind:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn keychain_input_quotes_the_key_as_one_interactive_argument() {
        assert_eq!(
            super::security_interactive_argument("dummy \\\"key"),
            Ok("\"dummy \\\\\\\"key\"".into())
        );
        assert!(super::security_interactive_argument("dummy\ncommand").is_err());
    }

    #[test]
    fn agent_ids_roundtrip() {
        for agent in super::ALL {
            assert_eq!(AgentKind::parse(agent.id()), Ok(agent));
        }
        assert!(AgentKind::parse("unknown").is_err());
    }

    /// Every agent starts one way or the other, never both: an npm adapter
    /// the app provisions, or a native CLI the app only finds.
    #[test]
    fn each_agent_is_adapter_hosted_or_native() {
        for agent in super::ALL {
            assert_eq!(agent.adapter().is_some(), agent.native_command().is_none());
            assert_eq!(agent.adapter().is_some(), agent.adapter_bin().is_some());
        }
    }

    #[test]
    fn adapter_runtime_manifest_and_lock_match_the_spawn_pins() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../adapter-runtime/package.json")).unwrap();
        let lock: serde_json::Value =
            serde_json::from_str(include_str!("../../adapter-runtime/package-lock.json")).unwrap();
        let locked = &lock["packages"][""]["dependencies"];

        for agent in super::ALL {
            let Some(adapter) = agent.adapter() else {
                continue;
            };
            let (package, version) = adapter.rsplit_once('@').unwrap();
            assert_eq!(manifest["dependencies"][package], version);
            assert_eq!(locked[package], version);
        }
    }

    /// Grok's installer links `~/.local/bin/agent` to Grok, so a machine
    /// with only that name has no Cursor; Cursor's installer always adds
    /// `cursor-agent` beside it.
    #[test]
    fn cursor_is_found_by_its_own_name_not_groks_agent_alias() {
        let home = std::env::temp_dir().join(format!("nurb-cursor-bin-{}", std::process::id()));
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = crate::platform::exe;
        std::fs::write(bin.join(exe("agent")), b"").unwrap();
        let (name, _) = AgentKind::Cursor.native_command().unwrap();
        assert_eq!(super::find_bin(name, &home, &[".local/bin"], None), None);

        std::fs::write(bin.join(exe("cursor-agent")), b"").unwrap();
        assert_eq!(
            super::find_bin(name, &home, &[".local/bin"], None),
            Some(bin.join(exe("cursor-agent")))
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Homebrew's cask puts claude in /opt/homebrew/bin, outside every
    /// installer spot, so it is found through the PATH the shell reports.
    #[test]
    fn a_cli_outside_the_installer_spots_is_found_on_the_path() {
        let root = std::env::temp_dir().join(format!("nurb-brew-bin-{}", std::process::id()));
        let (home, brew) = (root.join("home"), root.join("homebrew/bin"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&brew).unwrap();
        let claude = crate::platform::exe("claude");
        std::fs::write(brew.join(&claude), b"").unwrap();
        let path = std::env::join_paths([std::path::PathBuf::from("/usr/bin"), brew.clone()]).unwrap();
        assert_eq!(
            super::find_bin("claude", &home, &[".local/bin", ".claude/local"], Some(path)),
            Some(brew.join(&claude))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `agent_login` hands Codex a CODEX_PATH pointing at `.bin/codex`, which
    /// only exists because the adapter depends on a Codex CLI that installs
    /// under that name. An adapter bump that dropped it would put the login
    /// back on the bare PATH lookup that broke it.
    #[test]
    fn the_codex_adapter_installs_the_cli_its_login_is_pointed_at() {
        let lock: serde_json::Value =
            serde_json::from_str(include_str!("../../adapter-runtime/package-lock.json")).unwrap();
        let codex = &lock["packages"]["node_modules/@openai/codex"];
        assert!(codex["bin"]["codex"].is_string(), "no codex bin: {codex}");
    }
}
