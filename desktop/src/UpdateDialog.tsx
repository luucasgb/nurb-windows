import { useEffect, useRef } from "react";
import { Button } from "@nurb/ui";

// What Check for Updates found, said in the app's own dialog rather than a
// system message box, which looks foreign on every platform and chimes on
// Windows. The shell is ConfirmDialog's: a native <dialog> for the top layer,
// Escape and focus containment, with the same panel, type and footer. Mount it
// to open it.

export type UpdateCheck =
  | { kind: "current" }
  | { kind: "ready"; version: string }
  | { kind: "failed"; reason: string };

export default function UpdateDialog({
  check,
  onInstall,
  onClose,
}: {
  check: UpdateCheck;
  onInstall: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const firstRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    ref.current?.showModal();
    firstRef.current?.focus();
  }, []);

  const title =
    check.kind === "current"
      ? "You're up to date"
      : check.kind === "ready"
        ? `nurb ${check.version} is ready`
        : "Couldn't check for updates";
  // The updater's own error is for a bug report, not a sentence, so it rides
  // underneath in small type. A feed with no entry for this platform is the
  // one failure worth naming: it means no build has been published here yet.
  const unpublished = check.kind === "failed" && /fallback platforms|`platforms` object/.test(check.reason);
  const body =
    check.kind === "current"
      ? "This is the newest version of nurb."
      : check.kind === "ready"
        ? "nurb restarts to finish installing it. Your parts are saved as they are."
        : unpublished
          ? "No updates are published for this computer yet. This version keeps working as it is."
          : "nurb couldn't reach its update feed. Try again later.";

  return (
    <dialog
      ref={ref}
      aria-labelledby="update-title"
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      className="panel m-auto w-[320px] max-w-[calc(100vw-32px)] p-0 text-ink transition-[opacity,scale] duration-[240ms] backdrop:bg-scrim/30 backdrop:transition-colors backdrop:duration-[240ms] starting:scale-[0.98] starting:opacity-0 starting:backdrop:bg-scrim/0"
    >
      <div className="flex flex-col gap-1.5 px-4 pt-4 pb-3.5">
        <h2 id="update-title" className="font-display text-[16px] leading-5 font-bold tracking-[-0.02em] text-ink">
          {title}
        </h2>
        <p className="text-xs leading-4 break-words text-muted">{body}</p>
        {check.kind === "failed" ? (
          <p className="mono-caption mt-1 break-words text-faint">{check.reason}</p>
        ) : null}
      </div>
      <div className="flex justify-end gap-1.5 border-t border-line-soft px-4 py-3">
        {check.kind === "ready" ? (
          <>
            {/* Later takes focus first, so a stray Enter never restarts the app. */}
            <Button ref={firstRef} variant="outline" onClick={onClose}>
              Later
            </Button>
            <Button variant="accent" onClick={onInstall}>
              Restart &amp; Update
            </Button>
          </>
        ) : (
          <Button ref={firstRef} variant="ink" onClick={onClose}>
            OK
          </Button>
        )}
      </div>
    </dialog>
  );
}
