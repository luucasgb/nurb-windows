// What the shell says differs by OS in a few words only.
export const isWindows = typeof navigator !== "undefined" && navigator.userAgent.includes("Windows");

// "this Mac" on macOS, "this computer" on Windows.
export const computer = isWindows ? "computer" : "Mac";

// The OS line in About and setup reports. The engine's about_info answers with
// the bare macOS version ("15.1"), or the whole name on Windows ("Windows 11
// 24H2 (26100)"), so only the macOS one needs its name in front.
export const osName = (version: string) => (version.startsWith("Windows") ? version : `macOS ${version}`);
