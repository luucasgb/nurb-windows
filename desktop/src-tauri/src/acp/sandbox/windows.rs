//! The Windows half of the agent sandbox, with the same contract as the
//! Seatbelt profile: read almost anything, network allowed, write only where
//! the app says.
//!
//! Windows has no sandbox-exec, so nurb.exe plays that part itself. `wrap`
//! turns an adapter invocation into `nurb.exe --nurb-sandbox --write <root>...
//! -- <program> <args>`, and that helper starts the program under a
//! write-restricted token: every write is checked twice, once against the
//! user as usual and once against a short list of restricting SIDs, and both
//! must allow it. The list holds a SID only nurb uses, the logon session and
//! Everyone. The nurb SID is granted modify rights on the writable roots and
//! nothing else, so a write anywhere else in the user's profile fails at the
//! kernel exactly as it does under Seatbelt, and lands in the agent's own
//! transcript instead of a dialog. Reads are not restricted, as on macOS.
//! This is the model OpenAI's Codex ships as its unelevated Windows sandbox,
//! down to the token flags and the default DACL that keeps pipes working.
//!
//! The helper also owns the child's lifetime: the child runs in a job object
//! that kills everything in it when the helper's handle closes, so killing
//! the helper (which is what the app's process-group kill does on Windows)
//! takes the agent and every command it started, the same way killpg does.
//!
//! What differs from macOS, on purpose and documented:
//! - Reads of Documents, Desktop and Downloads are not refused. On macOS those
//!   rules exist to stop privacy prompts, which Windows does not have; a
//!   write-restricted token cannot restrict reads at all.
//! - The grants are ACL entries, so they stay on the folders after the app
//!   quits. They name only nurb's SID, which no other process carries, so
//!   they widen nothing for anyone else.
//!
//! No entry is user-managed: every writable root is computed at spawn time
//! from facts the app already owns, exactly as on macOS.

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
    SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation,
    GetTokenInformation, SetTokenInformation, TokenDefaultDacl, TokenGroups, TokenUser,
    WinWorldSid, AclSizeInformation, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, LUA_TOKEN,
    NO_INHERITANCE, OBJECT_INHERIT_ACE, SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    TOKEN_DEFAULT_DACL, TOKEN_GROUPS, TOKEN_USER, WRITE_RESTRICTED,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, ResumeThread,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, INFINITE,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
};

/// The first argument that turns nurb.exe into the sandbox helper.
pub(crate) const FLAG: &str = "--nurb-sandbox";

/// Read, write, execute and delete: what the agent needs inside a root.
const MODIFY: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const TOKEN_ALL_ACCESS: u32 = 0x000F_01FF;
/// What the helper exits with when it refuses to start the program: it never
/// falls back to running it unsandboxed.
const REFUSED: i32 = 126;

/// The helper invocation for `program args`: the same shape sandbox-exec has
/// on macOS, so the caller's pid, pipes and kill semantics are unchanged.
pub(crate) fn wrap(
    program: String,
    args: Vec<String>,
    project: &Path,
    engine_root: &Path,
) -> (String, Vec<String>) {
    let helper = std::env::current_exe()
        .map(|exe| exe.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "nurb.exe".into());
    ensure_agent_home(&program, &args);
    let mut wrapped = vec![FLAG.to_string()];
    for root in writable_roots(project, engine_root) {
        wrapped.push("--write".into());
        wrapped.push(root.to_string_lossy().into_owned());
    }
    for file in writable_files() {
        wrapped.push("--file".into());
        wrapped.push(file.to_string_lossy().into_owned());
    }
    wrapped.push("--temp".into());
    wrapped.push(agent_temp().to_string_lossy().into_owned());
    wrapped.push("--".into());
    wrapped.push(program);
    wrapped.extend(args);
    (helper, wrapped)
}

/// The temp folder sandboxed processes get as TEMP and TMP. The user's own
/// temp folder holds every other app's files, so the agent gets one of its
/// own inside it rather than a grant on all of it.
fn agent_temp() -> PathBuf {
    std::env::temp_dir().join("nurb-agent")
}

/// The same app-derived roots as the macOS profile, in Windows' places.
/// Roots that do not exist are skipped by the helper.
fn writable_roots(project: &Path, engine_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![project.to_path_buf(), engine_root.to_path_buf()];
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        // npm's and uv's caches live under Local AppData on Windows.
        roots.push(local.join("npm-cache"));
        roots.push(local.join("uv").join("cache"));
    }
    if let Some(home) = crate::platform::home() {
        roots.push(home.join(".cache"));
        roots.push(home.join(".npm"));
        roots.push(home.join(".config").join("nurb"));
        // The agents' own state, one folder each, as on macOS.
        for dot in AGENT_HOMES {
            roots.push(home.join(dot));
        }
    }
    roots
}

const AGENT_HOMES: [&str; 5] = [".claude", ".codex", ".gemini", ".cursor", ".grok"];

/// Agent state that is a file in the home folder rather than a folder of its
/// own: Claude keeps `~/.claude.json` beside `~/.claude`. macOS's prefix rule
/// covers it; here it is granted by name.
fn writable_files() -> Vec<PathBuf> {
    crate::platform::home()
        .map(|home| vec![home.join(".claude.json"), home.join(".claude.json.backup")])
        .unwrap_or_default()
}

/// An agent's state folder has to exist before it can be granted, and the
/// home folder itself stays shut, so an agent could never create its own.
/// The folder of the agent being started is made here; the others are left
/// alone so a machine never grows dot folders for agents it does not use.
fn ensure_agent_home(program: &str, args: &[String]) {
    let Some(home) = crate::platform::home() else {
        return;
    };
    let line = std::iter::once(program)
        // Two, because the dev checkout's `npx -y <package>` names the agent second.
        .chain(args.iter().map(String::as_str).take(2))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    for dot in AGENT_HOMES {
        if line.contains(&dot[1..]) {
            let _ = std::fs::create_dir_all(home.join(dot));
        }
    }
}

/// Entry point for nurb.exe's helper mode: Some(exit code) when the process
/// was started as the helper, None for a normal launch.
pub(crate) fn exec_from_args() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().map(OsString::as_os_str) != Some(OsStr::new(FLAG)) {
        return None;
    }
    Some(match Invocation::parse(&args[1..]) {
        Ok(invocation) => match invocation.run() {
            Ok(code) => code as i32,
            Err(why) => {
                eprintln!("nurb sandbox: refused to start {}: {why}", invocation.program.to_string_lossy());
                REFUSED
            }
        },
        Err(why) => {
            eprintln!("nurb sandbox: {why}");
            REFUSED
        }
    })
}

struct Invocation {
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    temp: Option<PathBuf>,
    program: OsString,
    args: Vec<OsString>,
}

impl Invocation {
    fn parse(args: &[OsString]) -> Result<Self, String> {
        let mut roots = Vec::new();
        let mut files = Vec::new();
        let mut temp = None;
        let mut rest = args.iter();
        loop {
            let Some(arg) = rest.next() else {
                return Err("no program after --".into());
            };
            match arg.to_str() {
                Some("--write") => roots.push(PathBuf::from(rest.next().ok_or("--write needs a path")?)),
                Some("--file") => files.push(PathBuf::from(rest.next().ok_or("--file needs a path")?)),
                Some("--temp") => temp = Some(PathBuf::from(rest.next().ok_or("--temp needs a path")?)),
                Some("--") => break,
                _ => return Err(format!("unknown argument {}", arg.to_string_lossy())),
            }
        }
        let program = rest.next().ok_or("no program after --")?.clone();
        Ok(Self {
            roots,
            files,
            temp,
            program,
            args: rest.cloned().collect(),
        })
    }

    /// Grant, then start under the restricted token and wait. The first two
    /// roots are the project and the engine's home: if either cannot be
    /// granted the agent could not work, so that refuses rather than starting
    /// a child bound to fail on every write.
    fn run(&self) -> Result<u32, String> {
        let sid = Sid::nurb()?;
        if let Some(temp) = &self.temp {
            std::fs::create_dir_all(temp).map_err(|e| format!("cannot create {}: {e}", temp.display()))?;
            grant(temp, &sid, true)?;
            std::env::set_var("TEMP", temp);
            std::env::set_var("TMP", temp);
        }
        for (index, root) in self.roots.iter().enumerate() {
            if !root.exists() {
                continue;
            }
            if let Err(why) = grant(root, &sid, true) {
                if index < 2 {
                    return Err(why);
                }
                eprintln!("nurb sandbox: {why}; continuing without it");
            }
        }
        for file in self.files.iter().filter(|file| file.is_file()) {
            if let Err(why) = grant(file, &sid, false) {
                eprintln!("nurb sandbox: {why}; continuing without it");
            }
        }
        let command_line = command_line(&resolve(&self.program), &self.args);
        let stdio = unsafe {
            [
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            ]
        };
        run_restricted(&sid, &command_line, stdio)
    }
}

/// A bare `npx` (the dev checkout's adapters) is `npx.cmd` on Windows, which
/// CreateProcess cannot start by itself; it goes through cmd.exe instead.
/// Everything the shipped app runs is an absolute .exe and passes through.
fn resolve(program: &OsStr) -> Vec<OsString> {
    let path = Path::new(program);
    let found = if path.is_absolute() || path.extension().is_some() {
        Some(path.to_path_buf())
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                ["exe", "cmd", "bat"]
                    .iter()
                    .map(|ext| dir.join(program).with_extension(ext))
                    .find(|candidate| candidate.is_file())
            })
        })
    };
    let program = found.unwrap_or_else(|| path.to_path_buf());
    let script = program
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if script {
        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        vec![shell, "/d".into(), "/c".into(), program.into_os_string()]
    } else {
        vec![program.into_os_string()]
    }
}

/// One Windows command line from argv, quoted the way CommandLineToArgvW and
/// the C runtime read it back.
fn command_line(program: &[OsString], args: &[OsString]) -> Vec<u16> {
    let mut line: Vec<u16> = Vec::new();
    for (index, arg) in program.iter().chain(args).enumerate() {
        if index > 0 {
            line.push(b' ' as u16);
        }
        quote_into(&mut line, arg);
    }
    line.push(0);
    line
}

fn quote_into(line: &mut Vec<u16>, arg: &OsStr) {
    let wide: Vec<u16> = arg.encode_wide().collect();
    let plain = !wide.is_empty()
        && !wide.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16 || c == 0x0b || c == b'"' as u16);
    if plain {
        line.extend(wide);
        return;
    }
    line.push(b'"' as u16);
    let mut backslashes = 0usize;
    for c in wide {
        if c == b'\\' as u16 {
            backslashes += 1;
            continue;
        }
        if c == b'"' as u16 {
            // Backslashes before a quote are doubled, and the quote escaped.
            line.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2 + 1));
        } else {
            line.extend(std::iter::repeat(b'\\' as u16).take(backslashes));
        }
        backslashes = 0;
        line.push(c);
    }
    // Before the closing quote, backslashes are doubled so it stays a quote.
    line.extend(std::iter::repeat(b'\\' as u16).take(backslashes * 2));
    line.push(b'"' as u16);
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn last_error(what: &str) -> String {
    format!("{what} failed: {}", std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32))
}

/// A SID in a buffer this process owns.
struct Sid(Vec<u8>);

impl Sid {
    fn ptr(&self) -> *mut c_void {
        self.0.as_ptr() as *mut c_void
    }

    fn from_string(text: &str) -> Result<Self, String> {
        let wide: Vec<u16> = OsStr::new(text).encode_wide().chain(Some(0)).collect();
        let mut sid: *mut c_void = std::ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
            return Err(last_error("ConvertStringSidToSid"));
        }
        let bytes = unsafe { sid_bytes(sid) };
        unsafe {
            LocalFree(sid);
        }
        Ok(Self(bytes))
    }

    /// The SID only nurb's sandbox carries. Derived from the user's own SID,
    /// so it is the same on every launch and every reinstall (the grants it
    /// already holds stay valid, and an uninstaller can name it) while two
    /// accounts on one machine never share it. S-1-5-21 SIDs are what Codex
    /// uses for the same purpose; 96 bits of hash make a collision with a
    /// real account moot.
    fn nurb() -> Result<Self, String> {
        let user = user_sid_string()?;
        let digest = Sha256::digest(format!("dev.nurb.desktop sandbox-write {user}").as_bytes());
        let part = |i: usize| u32::from_le_bytes([digest[i], digest[i + 1], digest[i + 2], digest[i + 3]]);
        Self::from_string(&format!("S-1-5-21-{}-{}-{}-{}", part(0), part(4), part(8), 1000 + part(12) % 1_000_000))
    }

    fn everyone() -> Result<Self, String> {
        let mut buffer = vec![0u8; 68];
        let mut size = buffer.len() as u32;
        if unsafe { CreateWellKnownSid(WinWorldSid, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size) } == 0 {
            return Err(last_error("CreateWellKnownSid"));
        }
        buffer.truncate(size as usize);
        Ok(Self(buffer))
    }
}

/// A SID's bytes, copied out of memory someone else owns.
unsafe fn sid_bytes(sid: *mut c_void) -> Vec<u8> {
    // SubAuthorityCount at offset 1; 8 bytes of header, 4 per sub-authority.
    let count = *(sid as *const u8).add(1) as usize;
    std::slice::from_raw_parts(sid as *const u8, 8 + 4 * count).to_vec()
}

fn current_token() -> Result<OwnedHandle, String> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut token) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    Ok(OwnedHandle(token))
}

fn token_info(token: HANDLE, class: i32) -> Result<Vec<u8>, String> {
    let mut size = 0u32;
    unsafe {
        GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut size);
    }
    // u64 storage keeps the structures inside aligned.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    if unsafe { GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), size, &mut size) } == 0 {
        return Err(last_error("GetTokenInformation"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, size as usize) };
    Ok(bytes.to_vec())
}

fn user_sid_string() -> Result<String, String> {
    let token = current_token()?;
    let info = token_info(token.0, TokenUser)?;
    let user = unsafe { &*(info.as_ptr() as *const TOKEN_USER) };
    let mut text: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(last_error("ConvertSidToStringSid"));
    }
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe {
        LocalFree(text.cast());
    }
    Ok(value)
}

/// The logon session's SID: the desktop, the window station and the pipes a
/// process makes are granted to it, so a token restricted without it cannot
/// even start.
fn logon_sid(token: HANDLE) -> Result<Sid, String> {
    let info = token_info(token, TokenGroups)?;
    let groups = unsafe { &*(info.as_ptr() as *const TOKEN_GROUPS) };
    let entries = unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    entries
        .iter()
        .find(|group| group.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID)
        .map(|group| Sid(unsafe { sid_bytes(group.Sid) }))
        .ok_or_else(|| "the logon SID is not on this token".into())
}

/// Gives nurb's SID modify rights on `path`, inherited by everything inside
/// when it is a folder. A grant that is already there, directly or inherited
/// from a granted parent, is left alone, so this costs one read per root on
/// every launch after the first.
fn grant(path: &Path, sid: &Sid, folder: bool) -> Result<(), String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(format!(
            "cannot read the permissions of {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(status as i32)
        ));
    }
    let result = (|| {
        if unsafe { already_granted(dacl, sid, folder) } {
            return Ok(());
        }
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: MODIFY,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if folder { SUB_CONTAINERS_AND_OBJECTS_INHERIT } else { NO_INHERITANCE },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.ptr().cast(),
            },
        };
        let mut updated: *mut ACL = std::ptr::null_mut();
        let status = unsafe { SetEntriesInAclW(1, &access, dacl, &mut updated) };
        if status != 0 {
            return Err(format!("cannot extend the permissions of {}: error {status}", path.display()));
        }
        // Writing the folder's DACL also propagates the inheritable entry to
        // what is already inside it.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                updated,
                std::ptr::null(),
            )
        };
        unsafe {
            LocalFree(updated.cast());
        }
        if status != 0 {
            return Err(format!(
                "cannot grant the sandbox {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}

unsafe fn already_granted(dacl: *mut ACL, sid: &Sid, folder: bool) -> bool {
    if dacl.is_null() {
        return false;
    }
    let mut size: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        dacl,
        (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return false;
    }
    let inherits = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
    (0..size.AceCount).any(|index| {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if GetAce(dacl, index, &mut ace) == 0 {
            return false;
        }
        let header = &*(ace as *const ACE_HEADER);
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            return false;
        }
        let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
        let sid_start = std::ptr::addr_of!(allowed.SidStart) as *mut c_void;
        allowed.Mask & MODIFY == MODIFY
            && EqualSid(sid_start, sid.ptr()) != 0
            && (!folder || header.AceFlags & inherits == inherits)
    })
}

/// The write-restricted token: the user's own token with every privilege but
/// traversal dropped, and nurb's SID, the logon session and Everyone as the
/// restricting list. Objects the child creates (pipes above all) default to
/// the logon session, which is on that list, so the child's own IPC works.
fn restricted_token(sid: &Sid) -> Result<OwnedHandle, String> {
    let base = current_token()?;
    let logon = logon_sid(base.0)?;
    let everyone = Sid::everyone()?;
    let restricting = [sid, &logon, &everyone].map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid.ptr(),
        Attributes: 0,
    });
    let mut token: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        CreateRestrictedToken(
            base.0,
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            restricting.len() as u32,
            restricting.as_ptr(),
            &mut token,
        )
    };
    if ok == 0 {
        return Err(last_error("CreateRestrictedToken"));
    }
    let token = OwnedHandle(token);
    set_default_dacl(token.0, &logon)?;
    Ok(token)
}

fn set_default_dacl(token: HANDLE, logon: &Sid) -> Result<(), String> {
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: logon.ptr().cast(),
        },
    };
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &access, std::ptr::null(), &mut dacl) };
    if status != 0 {
        return Err(format!("cannot build the default DACL: error {status}"));
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
    let ok = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            (&info as *const TOKEN_DEFAULT_DACL).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    };
    unsafe {
        LocalFree(dacl.cast());
    }
    if ok == 0 {
        return Err(last_error("SetTokenInformation(TokenDefaultDacl)"));
    }
    Ok(())
}

/// Starts the command line under the restricted token with the given stdio,
/// inside a job that dies with this process, and waits for its exit code.
fn run_restricted(sid: &Sid, command_line: &[u16], stdio: [HANDLE; 3]) -> Result<u32, String> {
    let token = restricted_token(sid)?;
    for handle in stdio {
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            }
        }
    }
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(last_error("CreateJobObject"));
    }
    let job = OwnedHandle(job);
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetInformationJobObject"));
    }

    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = stdio[0];
    startup.hStdOutput = stdio[1];
    startup.hStdError = stdio[2];
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let mut line = command_line.to_vec();
    let ok = unsafe {
        CreateProcessAsUserW(
            token.0,
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if ok == 0 {
        let program = String::from_utf16_lossy(&command_line[..command_line.len().saturating_sub(1)]);
        return Err(format!("{} ({program})", last_error("CreateProcessAsUser")));
    }
    let child = OwnedHandle(process.hProcess);
    let thread = OwnedHandle(process.hThread);
    // Assigned while suspended, so nothing it starts can slip outside the job.
    if unsafe { AssignProcessToJobObject(job.0, child.0) } == 0 {
        let why = last_error("AssignProcessToJobObject");
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(child.0, 1);
        }
        return Err(why);
    }
    unsafe {
        ResumeThread(thread.0);
        WaitForSingleObject(child.0, INFINITE);
    }
    let mut code = 1u32;
    unsafe {
        GetExitCodeProcess(child.0, &mut code);
    }
    // Dropping `job` here ends anything the child left running behind it.
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nurb-sbx-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs a cmd.exe one-liner under the sandbox, after granting `roots`.
    fn sandboxed(roots: &[&Path], script: &str) -> u32 {
        let sid = Sid::nurb().unwrap();
        for root in roots {
            grant(root, &sid, true).unwrap();
        }
        // cmd.exe parses its own command line and knows no \" escapes; /s
        // strips the outer quotes and leaves the script's own intact.
        let line: Vec<u16> = OsStr::new(&format!("cmd.exe /d /s /c \"{script}\""))
            .encode_wide()
            .chain(Some(0))
            .collect();
        let stdio = unsafe {
            [
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            ]
        };
        run_restricted(&sid, &line, stdio).unwrap()
    }

    #[test]
    fn the_kernel_enforces_the_write_boundary() {
        // The test IS the security property: writes inside the granted root
        // succeed, writes beside it fail, reads work everywhere.
        let project = scratch("in");
        let outside = scratch("out");
        assert_eq!(sandboxed(&[&project], &format!("echo hi> \"{}\\inside.txt\"", project.display())), 0);
        assert!(project.join("inside.txt").is_file());
        assert_ne!(sandboxed(&[&project], &format!("echo hi> \"{}\\escape.txt\"", outside.display())), 0);
        assert!(!outside.join("escape.txt").exists());
        let hosts = r"C:\Windows\System32\drivers\etc\hosts";
        assert_eq!(sandboxed(&[&project], &format!("type {hosts} > NUL")), 0);
        // Deleting inside works too: an agent renames and removes its own files.
        assert_eq!(sandboxed(&[&project], &format!("del \"{}\\inside.txt\"", project.display())), 0);
        assert!(!project.join("inside.txt").exists());
        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn the_home_folder_stays_shut() {
        let project = scratch("home");
        let home = crate::platform::home().unwrap();
        let canary = home.join(format!("nurb-sbx-canary-{}", std::process::id()));
        assert_ne!(sandboxed(&[&project], &format!("echo hacked>> \"{}\"", canary.display())), 0);
        assert!(!canary.exists(), "the sandbox wrote into the home folder");
        std::fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn a_child_started_inside_inherits_the_walls() {
        // The agent runs commands of its own; they get the same token.
        let project = scratch("nested");
        let outside = scratch("nested-out");
        let script = format!(
            "cmd /d /c \"echo hi> \"{}\\nested.txt\"\" & cmd /d /c \"echo hi> \"{}\\escape.txt\"\"",
            project.display(),
            outside.display()
        );
        sandboxed(&[&project], &script);
        assert!(project.join("nested.txt").is_file());
        assert!(!outside.join("escape.txt").exists());
        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn a_grant_is_found_again_and_not_repeated() {
        let root = scratch("again");
        let sid = Sid::nurb().unwrap();
        grant(&root, &sid, true).unwrap();
        let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            );
            assert!(already_granted(dacl, &sid, true));
            LocalFree(descriptor);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_nurb_sid_is_stable() {
        assert_eq!(Sid::nurb().unwrap().0, Sid::nurb().unwrap().0);
    }

    #[test]
    fn command_lines_survive_quotes_and_spaces() {
        let line = |args: &[&str]| {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            let wide = command_line(&args[..1], &args[1..]);
            String::from_utf16(&wide[..wide.len() - 1]).unwrap()
        };
        assert_eq!(line(&["a.exe", "plain"]), "a.exe plain");
        assert_eq!(line(&["C:\\Program Files\\x.exe", ""]), "\"C:\\Program Files\\x.exe\" \"\"");
        assert_eq!(line(&["a", r#"{"fastMode":true}"#]), r#"a "{\"fastMode\":true}""#);
        assert_eq!(line(&["a", r"C:\dir\ "]), r#"a "C:\dir\ ""#);
        assert_eq!(line(&["a", r"trailing\"]), r"a trailing\");
        assert_eq!(line(&["a", r"spa ce\"]), r#"a "spa ce\\""#);
    }

    #[test]
    fn helper_arguments_round_trip() {
        let (_, args) = wrap(
            "C:\\x\\claude.exe".into(),
            vec!["--print".into(), "a b".into()],
            Path::new("C:\\proj"),
            Path::new("C:\\data"),
        );
        assert_eq!(args[0], FLAG);
        let parsed = Invocation::parse(&args[1..].iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        assert_eq!(parsed.roots[0], PathBuf::from("C:\\proj"));
        assert_eq!(parsed.roots[1], PathBuf::from("C:\\data"));
        assert_eq!(parsed.program, OsString::from("C:\\x\\claude.exe"));
        assert_eq!(parsed.args, vec![OsString::from("--print"), OsString::from("a b")]);
    }
}
