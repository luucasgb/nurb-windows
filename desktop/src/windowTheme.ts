import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { readThemeChoice } from "@nurb/ui";
import { isWindows } from "./platform";

let watchingGround = false;

// The native window keeps its own appearance: the traffic lights, and what the
// webview reports as prefers-color-scheme. Pin it to an explicit choice, or hand
// it back to macOS for "system", so the chrome and the page never disagree.
export function syncWindowTheme() {
  const choice = readThemeChoice();
  getCurrentWindow()
    .setTheme(choice === "system" ? null : choice)
    .catch(() => {});
  if (!isWindows) return;
  paintGround();
  // The page follows a system theme change by itself; the window's ground has
  // to be told.
  if (!watchingGround) {
    watchingGround = true;
    new MutationObserver(paintGround).observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
  }
}

// Windows fills any part of the window WebView2 has not painted yet with the
// window's own background: the strip a window saved maximized uncovers as it
// opens, and the edge a resize drags out. As configured that is the light
// stage, so keep it the stage of whichever theme the page shows.
function paintGround() {
  const stage = getComputedStyle(document.documentElement).getPropertyValue("--color-stage").trim();
  if (!stage) return;
  getCurrentWebviewWindow()
    .setBackgroundColor(stage)
    .catch(() => {});
}
