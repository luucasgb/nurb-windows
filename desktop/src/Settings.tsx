import { useEffect, useState, type CSSProperties, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  Button,
  ConfirmDialog,
  FloatingHeader,
  HeaderCell,
  HeaderDivider,
  HeaderStack,
  Icon,
  LoadingButton,
  Lockup,
  Menu,
  type MenuItem,
  PanelStrip,
  Stage,
  AddPrinterDialog,
  bedLine,
  cue,
  readSoundPrefs,
  readThemeChoice,
  SegmentedControl,
  setSoundEnabled,
  setSoundVolume,
  setThemeChoice,
  type ThemeChoice,
} from "@nurb/ui";
import {
  addPrinter,
  fetchPrinters,
  removePrinter,
  setDefaultPrinter,
  slicerName,
  type Workshop,
} from "@nurb/workbench/api";
import { connectCommands, maskToken } from "./connect";
import { folderName } from "./downloads";
import { formatBytes, type StorageLocations } from "./files";
import { readCrashReports, setCrashReports } from "./crashReports";
import { syncWindowTheme } from "./windowTheme";
import About from "./About";
import { AGENT_LABEL } from "./Chat";
import { computer, isWindows } from "./platform";

type SettingsAgent = {
  id: string;
  label: string;
  loggedIn: boolean | null;
  // Who it is signed in as and on what plan, when the agent says.
  account: string | null;
  detail: string | null;
  canSignOut: boolean;
};

// The three things Manage can do that need a yes first.
type AgentConfirm = { id: string; action: "signOut" | "switch" | "removeKey" };

export type SettingsSection = "agents" | "printers" | "connect" | "general" | "about";

// The nav at the left, in the order a new user meets them.
const SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: "agents", label: "Agents" },
  { id: "printers", label: "Printers" },
  { id: "connect", label: "Connect" },
  { id: "general", label: "General" },
  { id: "about", label: "About" },
];

export type AboutInfo = {
  appVersion: string;
  nurbVersion: string;
  occtVersion: string | null;
  osVersion: string;
  arch: string;
};

type Props = {
  // The agents installed on this Mac, so signing in lives with the rest of the
  // setup rather than beside the parts.
  agents: SettingsAgent[];
  agentStatusState: "loading" | "ready" | "error";
  signingIn: string | null;
  // The local endpoint an assistant on this Mac connects to, once it is up.
  serve: { url: string; port: number; token: string } | null;
  // Versions for the panel strip and the About section. Null until the shell has them.
  about: AboutInfo | null;
  // Optional, so the button only appears once the shell hands it the updater.
  onCheckUpdates?: () => void;
  // Which section opens first, so the printer row in the rail lands on Printers.
  initialSection?: SettingsSection;
  onSignIn: (id: string) => Promise<boolean>;
  onSignOut: (id: string) => Promise<void>;
  // Signs in as someone else: a new key for Gemini, a sign-out then a sign-in otherwise.
  onSwitch: (id: string) => Promise<boolean>;
  onMoreAgents: () => void;
  onClose: () => void;
};

// The slicer's path is a path; the person recognises the folder it sits in. For a Mac
// app that is the folder holding the .app, not the bundle's own MacOS folder. On
// Windows it is the install folder, "Bambu Studio".
function folderOf(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  const app = parts.findIndex((part) => part.endsWith(".app"));
  const at = app > 0 ? app - 1 : parts.length - 2;
  return at >= 0 ? parts[at] : path;
}

// A section title and the line under it, the pair every block on this screen opens with.
function SectionHead({ title, note }: { title: string; note?: string }) {
  return (
    <div className="flex flex-col gap-1">
      <h2 className="font-display text-[17px] leading-[22px] tracking-tight text-ink">{title}</h2>
      {note ? <p className="text-[13.5px] leading-[18px] text-muted">{note}</p> : null}
    </div>
  );
}

export default function Settings({
  agents,
  agentStatusState,
  signingIn,
  serve,
  about,
  onCheckUpdates,
  initialSection = "agents",
  onSignIn,
  onSignOut,
  onSwitch,
  onMoreAgents,
  onClose,
}: Props) {
  const [section, setSection] = useState<SettingsSection>(initialSection);
  // A menu item can ask for a section while the panel is already open (Help > About).
  useEffect(() => setSection(initialSection), [initialSection]);
  const [soundPrefs, setSoundPrefs] = useState(readSoundPrefs);
  const [theme, setTheme] = useState(readThemeChoice);
  // Null until the shell answers, so the switch never flashes the wrong way.
  const [crashReports, setCrashReportsShown] = useState<boolean | null>(null);
  // The rail's error line is on another screen, so a failed sign-in reports here.
  const [signInError, setSignInError] = useState<string | null>(null);
  // The agent a Manage action is running on, and what its row says meanwhile.
  const [agentBusy, setAgentBusy] = useState<{ id: string; label: string } | null>(null);
  const [agentConfirm, setAgentConfirm] = useState<AgentConfirm | null>(null);
  // General > Files. The app data size comes on its own, because counting it is slow.
  const [locations, setLocations] = useState<StorageLocations | null>(null);
  const [appDataBytes, setAppDataBytes] = useState<number | null>(null);
  const [filesError, setFilesError] = useState<string | null>(null);
  // One key per copyable row, so only the row you pressed says so.
  const [copied, setCopied] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);
  // The bench. Every mutation answers with the whole list, so the rows update in place.
  const [workshop, setWorkshop] = useState<Workshop | null>(null);
  const [addingPrinter, setAddingPrinter] = useState(false);
  const [printerBusy, setPrinterBusy] = useState(false);
  const [printerError, setPrinterError] = useState<string | null>(null);
  // One object so the rows read from a single non-null value.
  const connect = serve ? { ...connectCommands(serve), token: serve.token } : null;
  // The nav's trailing captions, present only once there is an answer to show.
  const signedIn = agents.filter((agent) => agent.loggedIn).length;
  const counts: Partial<Record<SettingsSection, string>> = {
    agents: agentStatusState === "ready" ? `${signedIn}/${agents.length}` : undefined,
    printers: workshop ? String(workshop.printers.length) : undefined,
  };

  useEffect(() => {
    let live = true;
    fetchPrinters()
      .then((said) => live && setWorkshop(said))
      .catch((e) => live && setPrinterError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, []);

  const printerAction = async (run: () => Promise<Workshop>) => {
    setPrinterBusy(true);
    setPrinterError(null);
    try {
      setWorkshop(await run());
      setAddingPrinter(false);
    } catch (e) {
      setPrinterError(e instanceof Error ? e.message : String(e));
    } finally {
      setPrinterBusy(false);
    }
  };

  // Read each time General opens, so a size or a folder changed meanwhile is current.
  useEffect(() => {
    if (section !== "general") return;
    let live = true;
    invoke<StorageLocations>("storage_locations")
      .then((said) => live && setLocations(said))
      .catch((e) => live && setFilesError(String(e)));
    invoke<number>("app_data_size")
      .then((bytes) => live && setAppDataBytes(bytes))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [section]);

  // The shell shows the folder picker itself, so only a folder the person
  // picked can ever be given to the page's file writes.
  const changeExportFolder = async (command: "choose_export_folder" | "reset_export_folder") => {
    setFilesError(null);
    try {
      const exports = await invoke<StorageLocations["exports"] | null>(command);
      if (exports) setLocations((current) => (current ? { ...current, exports } : current));
    } catch (e) {
      setFilesError(String(e));
    }
  };

  const showLocation = (which: "library" | "exports" | "appData") => {
    setFilesError(null);
    invoke("show_location", { which }).catch((e) => setFilesError(String(e)));
  };

  const agentName = (id: string) => AGENT_LABEL[id] ?? agents.find((agent) => agent.id === id)?.label ?? id;

  const runAgentAction = async (id: string, label: string, run: () => Promise<unknown>) => {
    setSignInError(null);
    setAgentBusy({ id, label });
    try {
      if ((await run()) === true) cue("success");
    } catch (e) {
      setSignInError(String(e));
    } finally {
      setAgentBusy(null);
    }
  };

  const confirmAgent = (confirm: AgentConfirm) => {
    setAgentConfirm(null);
    if (confirm.action === "switch") {
      void runAgentAction(confirm.id, "Switching", () => onSwitch(confirm.id));
    } else {
      const label = confirm.action === "removeKey" ? "Removing key" : "Signing out";
      void runAgentAction(confirm.id, label, () => onSignOut(confirm.id));
    }
  };

  // Gemini's key is the app's own; every other agent has an account with its maker.
  const manageItems = (agent: SettingsAgent): MenuItem[] =>
    agent.id === "gemini"
      ? [
          {
            label: "Replace key",
            onSelect: () => void runAgentAction(agent.id, "Replacing key", () => onSwitch(agent.id)),
          },
          {
            label: "Remove key…",
            tone: "danger",
            separated: true,
            onSelect: () => setAgentConfirm({ id: agent.id, action: "removeKey" }),
          },
        ]
      : [
          { label: "Switch account…", onSelect: () => setAgentConfirm({ id: agent.id, action: "switch" }) },
          {
            label: "Sign out…",
            tone: "danger",
            separated: true,
            onSelect: () => setAgentConfirm({ id: agent.id, action: "signOut" }),
          },
        ];

  const confirmCopy = (confirm: AgentConfirm) => {
    const name = agentName(confirm.id);
    switch (confirm.action) {
      case "signOut":
        return {
          title: `Sign out of ${name}?`,
          body: `${name} shares this sign-in with your terminal, so it signs out there too. Open ${name} chats stop until you sign back in.`,
          label: "Sign out",
        };
      case "switch":
        return {
          title: `Switch ${name} account?`,
          body: `nurb signs ${name} out, here and in your terminal, then opens your browser. Sign in there with the account you want; if you close it first, ${name} stays signed out.`,
          label: "Switch account",
        };
      case "removeKey":
        return {
          title: "Remove the Gemini key?",
          body: "Gemini stops working in nurb until you add a key again. The key itself keeps working in Google AI Studio.",
          label: "Remove key",
        };
    }
  };

  // One Files row: what it is, where it is, and what can be done with it.
  const fileRow = (
    name: string,
    note: ReactNode,
    path: string | undefined,
    actions: ReactNode,
    last = false,
  ) => (
    <div className={`flex items-start justify-between gap-6 ${last ? "" : "border-b border-line-soft pb-2.5"}`}>
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="font-mono text-[11px] text-ink">{name}</span>
        <span className="text-[13.5px] leading-[18px] text-muted">{note}</span>
        <span className="truncate font-mono text-[11px] leading-4 text-ink-soft" title={path}>
          {path ?? "…"}
        </span>
      </div>
      <div className="flex shrink-0 items-center gap-2 pt-0.5">{actions}</div>
    </div>
  );

  const showIn = isWindows ? "Show in Explorer" : "Show in Finder";
  // Explorer counts in 1024s and Finder in 1000s; match the one beside it.
  const size = (bytes: number | null | undefined) =>
    bytes == null ? "" : ` ${formatBytes(bytes, isWindows ? 1024 : 1000)}.`;

  const copy = (key: string, text: string) => {
    writeText(text).then(() => {
      setCopied(key);
      setTimeout(() => setCopied((current) => (current === key ? null : current)), 1500);
    });
  };

  // A call rather than a component, so pressing it does not remount the button.
  const copyButton = (name: string, text: string) => (
    <Button aria-live="polite" onClick={() => copy(name, text)}>
      {copied === name ? "Copied" : "Copy"}
    </Button>
  );

  // Both controls preview a cue, so the choice is audible where it is made.
  useEffect(() => {
    readCrashReports().then(setCrashReportsShown, () => setCrashReportsShown(null));
  }, []);

  const toggleCrashReports = (on: boolean) => {
    setCrashReportsShown(on);
    setCrashReports(on).catch(() => setCrashReportsShown(!on));
  };

  const toggleSound = (on: boolean) => {
    setSoundEnabled(on);
    setSoundPrefs((prefs) => ({ ...prefs, enabled: on }));
    if (on) cue("ready");
  };

  const changeVolume = (volume: number) => {
    setSoundVolume(volume);
    setSoundPrefs((prefs) => ({ ...prefs, volume }));
    if (soundPrefs.enabled) cue("ready");
  };

  // A command and the button that copies it: label, raised block, action.
  const commandRow = (label: string, command: string, action?: ReactNode) => (
    <div className="flex flex-col gap-1.5">
      <div className="mono-label text-muted">{label}</div>
      <div className="flex items-center gap-2.5">
        <code className="grow overflow-hidden rounded-[6px] bg-raised px-3 py-2 font-mono text-[11px] leading-4 break-all text-ink-soft">
          {command}
        </code>
        {action}
        {copyButton(label, command)}
      </div>
    </div>
  );

  return (
    <Stage
      header={
        <FloatingHeader
          cells={
            <>
              {!isWindows && <div className="h-full w-[92px] shrink-0" data-tauri-drag-region />}
              {!isWindows && <HeaderDivider />}
              <HeaderCell>
                <Lockup />
              </HeaderCell>
              <HeaderDivider />
              <button
                type="button"
                onClick={onClose}
                className="chrome mono-label flex h-full items-center gap-2 px-4 text-muted transition-colors hover:text-ink"
              >
                <Icon name="chevron-left" />
                Library
              </button>
              <HeaderDivider />
              <HeaderStack label="NURB" value="Settings" />
            </>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1">
        <div className="panel mx-auto flex h-[620px] max-h-full w-full max-w-[920px] flex-col overflow-hidden">
          <PanelStrip label="Settings" trailing={about ? `nurb ${about.appVersion}` : undefined} />

          <div className="flex min-h-0 flex-1">
            <nav
              aria-label="Settings sections"
              className="flex w-[212px] shrink-0 flex-col gap-0.5 border-r border-line-soft px-2.5 py-3.5"
            >
              {SECTIONS.map((entry) => {
                const selected = entry.id === section;
                return (
                  <button
                    type="button"
                    key={entry.id}
                    aria-current={selected ? "page" : undefined}
                    onClick={() => setSection(entry.id)}
                    className={`flex h-[30px] w-full shrink-0 cursor-default items-center gap-2 rounded-[3px] px-2.5 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent ${
                      selected ? "bg-raised" : "hover:bg-raised/60"
                    } ${entry.id === "about" ? "mt-auto" : ""}`}
                  >
                    <span className="mono-value min-w-0 flex-1 truncate text-left text-ink uppercase">
                      {entry.label}
                    </span>
                    {counts[entry.id] ? (
                      <span className="mono-caption shrink-0 text-muted">{counts[entry.id]}</span>
                    ) : null}
                  </button>
                );
              })}
            </nav>

            <div className="min-w-0 flex-1 overflow-y-auto">
              {section === "agents" ? (
                <section className="flex flex-col gap-3.5 px-7 py-5.5">
                  <SectionHead
                    title="Agents"
                    note="The agent does the modelling. Sign in once; it stays signed in."
                  />
                  {agentStatusState === "loading" && (
                    <p className="mono-label text-muted" role="status">
                      Checking agents
                    </p>
                  )}
                  {agentStatusState === "error" && (
                    <p className="mono-label text-fail" role="alert">
                      Could not check the agents
                    </p>
                  )}
                  {agentStatusState === "ready" && (
                    <>
                      <div className="flex flex-col">
                        {agents.map((agent, index) => {
                          const who = agent.loggedIn ? [agent.account, agent.detail].filter(Boolean).join(" · ") : "";
                          // A sign-in started before Settings was last opened still owns its row.
                          const busy =
                            agentBusy?.id === agent.id
                              ? agentBusy.label
                              : signingIn === agent.id && agentBusy === null
                                ? "Signing in"
                                : null;
                          return (
                            <div
                              className={`flex h-10 shrink-0 items-center gap-3 ${
                                index === agents.length - 1 ? "" : "border-b border-line-soft"
                              }`}
                              key={agent.id}
                            >
                              <span className="w-16 shrink-0 font-mono text-[11px] text-ink">
                                {AGENT_LABEL[agent.id] ?? agent.label}
                              </span>
                              <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted" title={who || undefined}>
                                {who}
                              </span>
                              {busy ? (
                                <span className="btn-label text-muted" role="status">
                                  {busy}
                                </span>
                              ) : agent.loggedIn === false ? (
                                <LoadingButton
                                  label={agent.id === "gemini" ? "Add key" : "Sign in"}
                                  pendingLabel="Signing in"
                                  successLabel="Signed in"
                                  errorLabel="Try again"
                                  disabled={(signingIn !== null && signingIn !== agent.id) || agentBusy !== null}
                                  onAction={async () => {
                                    setSignInError(null);
                                    let signed: boolean;
                                    try {
                                      signed = await onSignIn(agent.id);
                                    } catch (e) {
                                      setSignInError(String(e));
                                      throw e;
                                    }
                                    // The key prompt was closed without a key, so
                                    // nothing was signed in and the button says so.
                                    if (!signed) throw new Error("No key");
                                    cue("success");
                                  }}
                                />
                              ) : agent.loggedIn ? (
                                <div className="flex shrink-0 items-center gap-3">
                                  <span className="btn-label flex items-center gap-1.5 text-pass">
                                    <Icon name="check" />
                                    Signed in
                                  </span>
                                  {agent.canSignOut ? (
                                    <Menu
                                      label={`Manage ${AGENT_LABEL[agent.id] ?? agent.label}`}
                                      disabled={agentBusy !== null || signingIn !== null}
                                      triggerClassName="h-6 rounded-control px-1.5 disabled:opacity-40"
                                      trigger={() => (
                                        <span className="btn-label flex items-center gap-1 text-muted transition-colors hover:text-ink">
                                          Manage
                                          <Icon name="chevron-down" />
                                        </span>
                                      )}
                                      menuClassName="w-[180px]"
                                      items={manageItems(agent)}
                                    />
                                  ) : null}
                                </div>
                              ) : (
                                <span className="btn-label text-muted" title={agent.detail ?? undefined}>
                                  Status unknown
                                </span>
                              )}
                            </div>
                          );
                        })}
                      </div>
                      <button
                        type="button"
                        className="chrome mono-label self-start text-accent-ink transition-colors hover:text-accent"
                        onClick={onMoreAgents}
                      >
                        + Add an agent
                      </button>
                    </>
                  )}
                  {signInError && (
                    <p className="mono-label text-fail" role="alert">
                      {signInError}
                    </p>
                  )}
                </section>
              ) : null}

              {section === "printers" ? (
                <section className="flex flex-col gap-3.5 px-7 py-5.5">
                  <SectionHead
                    title="Printers"
                    note="The machines on your bench. Checks use the bed size; slicing uses the slicer's own profile for it."
                  />
                  <div className="flex flex-col">
                    {(workshop?.printers ?? []).map((printer, index) => (
                      <div
                        className={`flex h-12 shrink-0 items-center gap-3 ${
                          index === (workshop?.printers.length ?? 0) - 1 ? "" : "border-b border-line-soft"
                        }`}
                        key={printer.name}
                      >
                        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                          <span className="truncate font-mono text-[11px] leading-4 text-ink">{printer.label}</span>
                          <span className="truncate font-mono text-[10px] leading-4 tracking-[0.08em] text-muted uppercase">
                            {bedLine(printer.bed)} MM · 0.4 nozzle
                          </span>
                        </div>
                        <div className="flex w-28 shrink-0 justify-end">
                          {printer.default ? (
                            <span className="flex items-center gap-1 rounded-[2px] border border-line px-1.5 py-0.5 font-mono text-[9px] leading-3 tracking-[0.08em] text-muted uppercase">
                              <Icon name="check" />
                              Default
                            </span>
                          ) : (
                            <button
                              type="button"
                              disabled={printerBusy}
                              onClick={() => void printerAction(() => setDefaultPrinter(printer.name))}
                              className="chrome font-mono text-[10px] leading-4 tracking-[0.08em] text-muted uppercase transition-colors hover:text-ink disabled:opacity-50"
                            >
                              Make default
                            </button>
                          )}
                        </div>
                        <div className="flex w-14 shrink-0 justify-end">
                          <button
                            type="button"
                            disabled={printerBusy}
                            onClick={() => void printerAction(() => removePrinter(printer.name))}
                            className="chrome font-mono text-[10px] leading-4 tracking-[0.08em] text-faint uppercase transition-colors hover:text-fail disabled:opacity-50"
                          >
                            Remove
                          </button>
                        </div>
                      </div>
                    ))}
                    {workshop && workshop.printers.length === 0 ? (
                      <p className="text-[13.5px] leading-[18px] text-muted">No printers yet.</p>
                    ) : null}
                  </div>
                  <button
                    type="button"
                    className="chrome mono-label self-start text-accent-ink transition-colors hover:text-accent"
                    onClick={() => setAddingPrinter(true)}
                  >
                    + Add a printer
                  </button>
                  {printerError ? (
                    <p className="mono-label text-fail" role="alert">
                      {printerError}
                    </p>
                  ) : null}

                  <div className="h-px bg-line-soft" />

                  <div className="flex items-start justify-between gap-6">
                    <div className="flex min-w-0 flex-col gap-1">
                      <span className="font-mono text-[10px] leading-4 tracking-[0.08em] text-muted uppercase">
                        Slicers
                      </span>
                      {workshop?.slicers.length ? (
                        workshop.slicers.map((found) => (
                          <span
                            key={found.path}
                            className="flex items-center gap-1.5 font-mono text-[11px] leading-4 text-ink"
                          >
                            <span className="text-pass">
                              <Icon name="check" />
                            </span>
                            {slicerName(found.name)} · in {folderOf(found.path)}
                          </span>
                        ))
                      ) : (
                        <span className="flex items-center gap-1.5 font-mono text-[11px] leading-4 text-ink">
                          <span className="text-warn">
                            <Icon name="warning" />
                          </span>
                          No slicer found
                        </span>
                      )}
                    </div>
                    {workshop?.slicers.length ? (
                      <p className="w-[240px] shrink-0 text-right text-[12px] leading-4 text-muted">
                        Each printer uses the slicer that has its profile, for weight, print time and Open in
                        slicer. Prusa printers use PrusaSlicer.
                      </p>
                    ) : (
                      <button
                        type="button"
                        onClick={() =>
                          openUrl("https://github.com/SoftFever/OrcaSlicer/releases").catch(() => {})
                        }
                        className="chrome w-[240px] shrink-0 text-right text-[12px] leading-4 text-muted underline underline-offset-2 transition-colors hover:text-ink"
                      >
                        Install OrcaSlicer, or PrusaSlicer for a Prusa, and weight and print time appear. Downloads work either way.
                      </button>
                    )}
                  </div>
                </section>
              ) : null}

              {section === "about" && about ? <About {...about} onCheckUpdates={onCheckUpdates} /> : null}

              {section === "general" ? (
                <section className="flex flex-col gap-3.5 px-7 py-5.5">
                  <SectionHead title="General" note={`How nurb behaves on this ${computer}.`} />
                  <div className="flex flex-col gap-2.5 border-y border-line-soft py-2.5">
                    <div className="flex items-center justify-between gap-6 border-b border-line-soft pb-2.5">
                      <div className="flex min-w-0 flex-col gap-0.5">
                        <span className="font-mono text-[11px] text-ink">Appearance</span>
                        <span className="text-[13.5px] leading-[18px] text-muted">
                          System matches your {computer}'s light or dark setting.
                        </span>
                      </div>
                      <SegmentedControl
                        label="Appearance"
                        options={[
                          { value: "system", label: "System" },
                          { value: "light", label: "Light" },
                          { value: "dark", label: "Dark" },
                        ]}
                        value={theme}
                        onValueChange={(value) => {
                          setThemeChoice(value as ThemeChoice);
                          syncWindowTheme();
                          setTheme(value as ThemeChoice);
                        }}
                        className="shrink-0"
                      />
                    </div>
                    <div className="flex items-center justify-between gap-6">
                      <div className="flex min-w-0 flex-col gap-0.5">
                        <span className="font-mono text-[11px] text-ink">Sounds</span>
                        <span className="text-[13.5px] leading-[18px] text-muted">
                          Short cues when a build finishes, a link is copied, or something needs you.
                        </span>
                      </div>
                      <button
                        type="button"
                        role="switch"
                        aria-checked={soundPrefs.enabled}
                        aria-label="Play sounds"
                        onClick={() => toggleSound(!soundPrefs.enabled)}
                        className={`flex h-[18px] w-8 shrink-0 items-center rounded-full px-0.5 transition-colors ${
                          soundPrefs.enabled ? "justify-end bg-accent" : "justify-start bg-line"
                        }`}
                      >
                        <span className="size-3.5 shrink-0 rounded-full bg-white" />
                      </button>
                    </div>
                    {soundPrefs.enabled ? (
                      <div className="flex items-center gap-3">
                        <span className="mono-label w-12 shrink-0 text-muted">Volume</span>
                        <input
                          type="range"
                          min={0}
                          max={1}
                          step={0.05}
                          value={soundPrefs.volume}
                          aria-label="Volume"
                          onChange={(event) => changeVolume(Number(event.target.value))}
                          style={{ "--pct": soundPrefs.volume } as CSSProperties}
                          className="param-slider grow"
                        />
                      </div>
                    ) : null}
                    {crashReports !== null ? (
                      <div className="flex items-center justify-between gap-6 border-t border-line-soft pt-2.5">
                        <div className="flex min-w-0 flex-col gap-0.5">
                          <span className="font-mono text-[11px] text-ink">Crash reports</span>
                          <span className="text-[13.5px] leading-[18px] text-muted">
                            When something breaks, send the error to the nurb developers so it gets fixed. Your parts and chats are never included.
                          </span>
                        </div>
                        <button
                          type="button"
                          role="switch"
                          aria-checked={crashReports}
                          aria-label="Send crash reports"
                          onClick={() => toggleCrashReports(!crashReports)}
                          className={`flex h-[18px] w-8 shrink-0 items-center rounded-full px-0.5 transition-colors ${
                            crashReports ? "justify-end bg-accent" : "justify-start bg-line"
                          }`}
                        >
                          <span className="size-3.5 shrink-0 rounded-full bg-white" />
                        </button>
                      </div>
                    ) : null}
                  </div>

                  <div className="mt-2">
                    <SectionHead
                      title="Files"
                      note="Where nurb keeps what you make. Your parts live in the library, not as separate files."
                    />
                  </div>
                  <div className="flex flex-col gap-2.5 border-y border-line-soft py-2.5">
                    {fileRow(
                      "Library",
                      `Your projects, parts and measurements.${size(locations?.library.bytes)}`,
                      locations?.library.path,
                      <Button onClick={() => showLocation("library")} disabled={!locations}>
                        {showIn}
                      </Button>
                    )}
                    {fileRow(
                      "Exports",
                      locations?.exports.missing ? (
                        <span className="text-warn" title={locations.exports.missing}>
                          {folderName(locations.exports.missing)} is not there, so files are saved to Downloads
                          until it is back.
                        </span>
                      ) : (
                        "Where downloaded parts and files for the slicer are saved."
                      ),
                      locations?.exports.path,
                      <>
                        {locations?.exports.custom || locations?.exports.missing ? (
                          <Button onClick={() => void changeExportFolder("reset_export_folder")}>Use Downloads</Button>
                        ) : null}
                        <Button onClick={() => void changeExportFolder("choose_export_folder")} disabled={!locations}>
                          Change…
                        </Button>
                        <Button onClick={() => showLocation("exports")} disabled={!locations}>
                          {showIn}
                        </Button>
                      </>
                    )}
                    {fileRow(
                      "App data",
                      `The engine, the agents and your chats.${size(appDataBytes)}`,
                      locations?.appData.path,
                      <Button onClick={() => showLocation("appData")} disabled={!locations}>
                        {showIn}
                      </Button>,
                      true
                    )}
                  </div>
                  {filesError ? (
                    <p className="mono-label text-fail" role="alert">
                      {filesError}
                    </p>
                  ) : null}
                </section>
              ) : null}

              {section === "connect" ? (
                <section className="flex flex-col gap-4 px-7 py-5.5">
                  <SectionHead
                    title="Connect another agent"
                    note="An agent outside nurb can open and build your parts at this address. Keep nurb open while you use it."
                  />
                  {!connect ? (
                    <p className="mono-label text-muted">Starting the engine</p>
                  ) : (
                    <>
                      <div className="flex flex-col gap-3">
                        <div className="flex items-end justify-between gap-4">
                          <div className="flex flex-col gap-[5px] overflow-hidden">
                            <div className="mono-label text-muted">Address</div>
                            <div className="truncate font-mono text-[12px] leading-4 text-ink">
                              {connect.mcpUrl}
                            </div>
                          </div>
                          {copyButton("address", connect.mcpUrl)}
                        </div>
                        <div className="flex items-end justify-between gap-4">
                          <div className="flex flex-col gap-[5px] overflow-hidden">
                            <div className="mono-label text-muted">Token</div>
                            <div className="truncate font-mono text-[12px] leading-4 text-ink">
                              {revealed ? connect.token : maskToken(connect.token)}
                            </div>
                          </div>
                          <div className="flex shrink-0 items-center gap-2">
                            <Button onClick={() => setRevealed(!revealed)}>
                              {revealed ? "Hide" : "Reveal"}
                            </Button>
                            {copyButton("token", connect.token)}
                          </div>
                        </div>
                      </div>
                      <div className="flex flex-col gap-2.5">
                        {commandRow("Claude Code", connect.claudeCode)}
                        {commandRow("Codex CLI", connect.codexCli)}
                        {commandRow(
                          "Cursor",
                          connect.cursorConfig,
                          <Button
                            variant="accent"
                            onClick={() => openUrl(connect.cursorLink).catch(() => {})}
                          >
                            Add to Cursor
                          </Button>
                        )}
                      </div>
                    </>
                  )}
                </section>
              ) : null}

            </div>
          </div>
        </div>
      </div>
      {agentConfirm ? (
        <ConfirmDialog
          title={confirmCopy(agentConfirm).title}
          confirmLabel={confirmCopy(agentConfirm).label}
          onConfirm={() => confirmAgent(agentConfirm)}
          onCancel={() => setAgentConfirm(null)}
        >
          {confirmCopy(agentConfirm).body}
        </ConfirmDialog>
      ) : null}
      {addingPrinter && workshop ? (
        <AddPrinterDialog
          catalogue={workshop.catalogue}
          owned={workshop.printers.map((printer) => printer.name)}
          busy={printerBusy}
          error={printerError}
          onAdd={(name) => void printerAction(() => addPrinter(name))}
          onClose={() => {
            setAddingPrinter(false);
            setPrinterError(null);
          }}
        />
      ) : null}
    </Stage>
  );
}
