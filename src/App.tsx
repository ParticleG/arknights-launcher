import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  GameExited, GameStatus, LaunchResult, Operation, OperationFinished,
  OperationProgress, ReleaseSummary, Settings,
} from "./protocol";

type Notice = { tone: "success" | "warning" | "error" | "info"; title: string; detail: string };
type Pending = "save" | "launch" | "picker" | null;
const number = new Intl.NumberFormat("en-US");

function bytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  const unit = Math.min(Math.floor(Math.log(value) / Math.log(1024)), 4);
  return `${(value / 1024 ** unit).toLocaleString("en-US", { maximumFractionDigits: unit > 0 ? 1 : 0 })} ${["B", "KiB", "MiB", "GiB", "TiB"][unit]}`;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function Icon({ name }: { name: "refresh" | "folder" | "check" | "arrow" | "download" }) {
  const paths = {
    refresh: "M19 8a7 7 0 1 0 0 8M19 3v5h-5",
    folder: "M3 6h6l2 3h10v10H3V6Z",
    check: "m5 12 4 4L19 6",
    arrow: "M5 12h14m-6-6 6 6-6 6",
    download: "M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5",
  };
  return <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={paths[name]} /></svg>;
}

export default function App() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [status, setStatus] = useState<GameStatus | null>(null);
  const [release, setRelease] = useState<ReleaseSummary | null>(null);
  const [ready, setReady] = useState(false);
  const [initializing, setInitializing] = useState(true);
  const [statusChecking, setStatusChecking] = useState(false);
  const [releaseChecking, setReleaseChecking] = useState(false);
  const [statusError, setStatusError] = useState("");
  const [releaseError, setReleaseError] = useState("");
  const [listenerError, setListenerError] = useState("");
  const [pending, setPending] = useState<Pending>(null);
  const [operation, setOperation] = useState<Operation | null>(null);
  const [progress, setProgress] = useState<OperationProgress | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const activeOperation = useRef<Operation | null>(null);
  const mounted = useRef(true);
  const statusRequest = useRef(0);
  const exitGeneration = useRef(0);
  const terminalFocus = useRef<HTMLDivElement>(null);

  const refreshStatus = useCallback(async () => {
    const request = ++statusRequest.current;
    setStatusChecking(true);
    try {
      const next = await invoke<GameStatus>("get_status");
      if (!mounted.current || request !== statusRequest.current) return;
      setStatus(next);
      setSettings(next.settings);
      setDraft(current => current ?? next.settings);
      setStatusError("");
    } catch (error) {
      if (mounted.current && request === statusRequest.current) setStatusError(errorText(error));
    } finally {
      if (mounted.current && request === statusRequest.current) setStatusChecking(false);
    }
  }, []);

  const checkRelease = useCallback(async () => {
    setReleaseChecking(true);
    setReleaseError("");
    try {
      const next = await invoke<ReleaseSummary>("check_release");
      if (mounted.current) setRelease(next);
    } catch (error) {
      if (mounted.current) setReleaseError(errorText(error));
    } finally {
      if (mounted.current) setReleaseChecking(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    const unlisten: UnlistenFn[] = [];
    const retain = (stop: UnlistenFn) => { if (disposed) stop(); else unlisten.push(stop); };
    async function initialize() {
      try {
        await Promise.all([
          listen<OperationProgress>("operation-progress", ({ payload }) => {
            if (!disposed && activeOperation.current === payload.operation) setProgress(payload);
          }).then(retain),
          listen<OperationFinished>("operation-finished", ({ payload }) => {
            if (disposed || activeOperation.current !== payload.operation) return;
            activeOperation.current = null;
            setOperation(null);
            setCancelling(false);
            if (payload.error) {
              setNotice({ tone: "error", title: payload.operation === "verify" ? "Verification failed" : "Installation failed", detail: payload.error });
            } else if (payload.cancelled) {
              setNotice({ tone: "warning", title: "Operation cancelled", detail: "Completed files are kept. Run verification or repair again when you are ready." });
            } else if (payload.operation === "verify" && payload.summary) {
              const summary = payload.summary;
              const changed = summary.missing_files + summary.changed_files;
              setNotice({
                tone: changed > 0 ? "warning" : "success",
                title: changed > 0 ? "Files need attention" : "All required files verified",
                detail: `${number.format(summary.files_checked)} files checked · ${bytes(summary.verified_bytes)} verified · ${number.format(summary.missing_files)} missing · ${number.format(summary.changed_files)} changed.${changed > 0 ? " Use repair to download only the affected files." : ` Version ${summary.version}.`}`,
              });
            } else {
              setNotice({ tone: "success", title: payload.operation === "install" ? "Installation is ready" : "Verification complete", detail: payload.version ? `Version ${payload.version} verified. Existing correct files were preserved.` : "The operation completed successfully." });
            }
            void refreshStatus();
            requestAnimationFrame(() => terminalFocus.current?.focus());
          }).then(retain),
          listen<GameExited>("game-exited", ({ payload }) => {
            if (disposed) return;
            exitGeneration.current += 1;
            if (!activeOperation.current) setNotice({
              tone: payload.exit_code !== null && payload.exit_code !== 0 ? "warning" : "info",
              title: "Game process exited",
              detail: payload.exit_code === null ? "The launcher could not read an exit code." : `Exit code: ${payload.exit_code}.`,
            });
            void refreshStatus();
          }).then(retain),
        ]);
        if (!disposed) setReady(true);
      } catch (error) {
        if (!disposed) setListenerError(`Native event listeners could not be registered: ${errorText(error)}. Restart the launcher before verifying or installing.`);
      }
      if (disposed) return;
      const [storedSettings] = await Promise.allSettled([
        invoke<Settings>("get_settings"),
        refreshStatus(),
      ]);
      if (disposed) return;
      setInitializing(false);
      if (storedSettings.status === "fulfilled") {
        setSettings(current => current ?? storedSettings.value);
        setDraft(current => current ?? storedSettings.value);
      }
      void checkRelease();
    }
    void initialize();
    return () => {
      disposed = true;
      mounted.current = false;
      for (const stop of unlisten) stop();
    };
  }, [checkRelease, refreshStatus]);

  const working = operation !== null;
  const locked = initializing || working || pending !== null || statusChecking;
  const dirty = draft !== null && (settings === null || draft.game_dir !== settings.game_dir
    || draft.compat_data !== settings.compat_data || draft.proton_path !== settings.proton_path);
  const settingsLocked = locked || !status || status.running;
  const actionsLocked = locked || dirty || !status;
  const canSave = draft !== null && Object.values(draft).every(value => value.trim().length > 0);
  const updateAvailable = release !== null && status?.verified_version !== null
    && status?.verified_version !== undefined && release.version !== status.verified_version;
  const installLabel = !status?.installed ? "Install game" : updateAvailable ? "Update / repair" : "Repair / update";
  const fraction = progress && progress.total_bytes > 0
    ? progress.completed_bytes / progress.total_bytes
    : progress && progress.total_files > 0 ? progress.completed_files / progress.total_files : null;
  const percent = fraction === null ? null : Math.min(100, Math.max(0, fraction * 100));

  async function saveSettings(event: FormEvent) {
    event.preventDefault();
    if (!draft || settingsLocked || !dirty || !canSave) return;
    setPending("save");
    setNotice(null);
    try {
      const next = await invoke<Settings>("save_settings", { settings: draft });
      setSettings(next);
      setDraft(next);
      await refreshStatus();
      setNotice({ tone: "success", title: "Paths saved", detail: "Launch and file operations will use this installation and runtime." });
    } catch (error) {
      setNotice({ tone: "error", title: "Could not save paths", detail: errorText(error) });
      await refreshStatus();
    } finally { setPending(null); }
  }

  async function choosePath(key: keyof Settings) {
    if (!draft || settingsLocked) return;
    setPending("picker");
    try {
      const selected = await open({
        title: key === "proton_path" ? "Choose the Proton executable" : key === "game_dir" ? "Choose the Arknights game folder" : "Choose the Proton compatdata folder",
        directory: key !== "proton_path",
        multiple: false,
        defaultPath: draft[key] || undefined,
      });
      if (typeof selected === "string") setDraft(current => current ? { ...current, [key]: selected } : current);
    } catch (error) {
      setNotice({ tone: "error", title: "Could not open file picker", detail: errorText(error) });
    } finally { setPending(null); }
  }

  async function startOperation(kind: Operation) {
    if (activeOperation.current || !ready || actionsLocked || releaseChecking || (kind === "install" && status?.running)) return;
    activeOperation.current = kind;
    setOperation(kind);
    setProgress(null);
    setNotice(null);
    setCancelling(false);
    try {
      await invoke(kind === "verify" ? "verify_installation" : "install_or_update");
    } catch (error) {
      if (activeOperation.current !== kind) return;
      activeOperation.current = null;
      setOperation(null);
      setCancelling(false);
      setNotice({ tone: "error", title: "Operation could not start", detail: errorText(error) });
      await refreshStatus();
      terminalFocus.current?.focus();
    }
  }

  async function cancelOperation() {
    if (!activeOperation.current || cancelling) return;
    setCancelling(true);
    try {
      await invoke("cancel_operation");
    } catch (error) {
      if (!activeOperation.current) return;
      setCancelling(false);
      setNotice({ tone: "error", title: "Cancellation failed", detail: errorText(error) });
    }
  }

  async function launch() {
    if (actionsLocked || !status?.installed || status.running || !settings?.proton_path) return;
    setPending("launch");
    setNotice(null);
    const generationBeforeLaunch = exitGeneration.current;
    try {
      const result = await invoke<LaunchResult>("launch_game");
      if (exitGeneration.current === generationBeforeLaunch) {
        setNotice({ tone: "info", title: result.already_running ? "Arknights is already running" : "Launch command started", detail: result.already_running ? "No additional game process was created." : "Proton is starting Arknights. The game opens in its own window." });
      }
      await refreshStatus();
    } catch (error) {
      setNotice({ tone: "error", title: "Could not launch Arknights", detail: errorText(error) });
      await refreshStatus();
    } finally { setPending(null); }
  }

  async function refresh() {
    if (locked || releaseChecking) return;
    await Promise.all([refreshStatus(), checkRelease()]);
  }

  async function officialWebsite() {
    try { await openUrl("https://ak.hypergryph.com/"); }
    catch (error) { setNotice({ tone: "error", title: "Could not open website", detail: errorText(error) }); }
  }

  return (
    <div className="app-shell">
      <header className="app-header">
        <div className="brand"><img src="/launcher.svg" width="38" height="38" alt="" /><div><strong>ARKNIGHTS <span>LAUNCHER</span></strong><small>LOCAL CLIENT / LINUX</small></div></div>
        <div className="header-meta"><span className="tag">CN · ORIGINAL</span><span className="version">v0.1.0</span></div>
      </header>

      <main>
        <section className="hero" aria-labelledby="game-title">
          <div className="hero-grid" aria-hidden="true"><div className="hero-coordinate">01 / CN</div><div className="hero-symbol">A<span> / </span>K</div><div className="hero-bar" /></div>
          <div className="hero-content"><p className="eyebrow">ORIGINAL ARKNIGHTS · CHINA CLIENT</p><h1 id="game-title">Ready when<br />you are<span>.</span></h1><p className="hero-description">Your installation. Your runtime.<br />A small native home for Arknights on Linux.</p></div>
          <div className="hero-bottom"><span className={`state-dot ${status?.running ? "live" : ""}`} /><span>{initializing ? "Reading local installation…" : statusChecking ? "Refreshing local status…" : status ? status.running ? "Game is running" : status.installed ? "Game installation detected" : "No game executable detected" : "Local status unavailable"}</span><span className="hero-platform">PROTON / NATIVE LAUNCHER</span></div>
        </section>

        <div className="workspace">
          <section className="panel installation" aria-labelledby="installation-heading">
            <div className="section-heading"><div><p className="eyebrow">01 / CLIENT</p><h2 id="installation-heading">Installation</h2></div><button type="button" onClick={() => void refresh()} disabled={locked || releaseChecking} aria-label="Refresh status and check updates"><Icon name="refresh" />{releaseChecking ? "Checking…" : "Check updates"}</button></div>
            <dl className="version-list">
              <div><dt>Verified version</dt><dd>{status?.verified_version ?? "Unknown"}</dd></div>
              <div><dt>Latest release</dt><dd>{releaseChecking ? <span className="muted">Checking…</span> : release?.version ?? "Unavailable"}</dd></div>
              <div><dt>Client version</dt><dd>{release?.client_version ?? "—"}</dd></div>
            </dl>
            <p className="installation-note">{status?.running ? "File changes are locked while the game is running. Read-only verification is available." : !status?.installed ? "Choose an existing game folder, or install the original CN client." : !status.verified_version ? "Existing installation detected. Its version is unknown until the required files pass verification. You can still launch it." : updateAvailable ? "A different release is available. Only missing or changed files will be downloaded." : "Verification checks original archive files. Unrelated extra files are preserved."}</p>
            {release && <div className="release-stats"><span><strong>{bytes(release.installed_bytes)}</strong>Installed size</span><span><strong>{number.format(release.file_count)}</strong>Archive files</span><span><strong>{bytes(release.compressed_bytes)}</strong>Full archive</span></div>}
            {releaseError && <div className="inline-warning" role="status"><strong>Release check unavailable</strong><p>{releaseError}</p><span>{release ? "Showing the last successful release check. " : ""}Local settings and launch remain available. Verify or repair can try the official service again.</span></div>}
            <div className="maintenance-actions"><button type="button" onClick={() => void startOperation("verify")} disabled={actionsLocked || !ready || releaseChecking}><Icon name="check" />Verify files</button><button type="button" onClick={() => void startOperation("install")} disabled={actionsLocked || !ready || releaseChecking || status?.running}><Icon name="download" />{installLabel}</button></div>
            <p className="microcopy">Verification is read-only. Repair downloads only affected files.</p>
          </section>

          <section className="panel paths" aria-labelledby="paths-heading">
            <div className="section-heading"><div><p className="eyebrow">02 / LOCAL SETUP</p><h2 id="paths-heading">Paths & runtime</h2></div>{dirty && <span className="unsaved">Unsaved changes</span>}</div>
            <form onSubmit={event => void saveSettings(event)}>
              {([
                ["game_dir", "Game folder", "The folder containing Arknights.exe."],
                ["compat_data", "Proton compatdata", "The compatdata directory containing pfx, not pfx itself."],
                ["proton_path", "Proton executable", "Select the proton executable from your installed runtime."],
              ] as const).map(([key, label, help]) => <div className="path-field" key={key}><label htmlFor={key}>{label}</label><div className="path-input"><input id={key} name={key} value={draft?.[key] ?? ""} disabled={settingsLocked} spellCheck={false} autoComplete="off" onChange={event => setDraft(current => current ? { ...current, [key]: event.target.value } : current)} aria-describedby={`${key}-help`} /><button type="button" className="icon-button" disabled={settingsLocked} onClick={() => void choosePath(key)} aria-label={`Browse for ${label.toLowerCase()}`} title={`Browse for ${label.toLowerCase()}`}><Icon name="folder" /></button></div><p id={`${key}-help`}>{help}</p></div>)}
              <div className="settings-actions"><span>{status?.running ? "Paths are locked while the game is running." : dirty ? "Save path changes before launching or maintaining files." : "Paths are stored only on this device."}</span><button type="button" className="quiet-button" disabled={!dirty || settingsLocked} onClick={() => setDraft(settings)}>Discard</button><button type="submit" className="save-button" disabled={!dirty || settingsLocked || !canSave}>{pending === "save" ? "Saving…" : "Save paths"}</button></div>
            </form>
          </section>
        </div>

        {(statusError || listenerError) && <div className="notice error" role="alert"><strong>{listenerError ? "Native event connection unavailable" : "Local status could not be read"}</strong><p>{listenerError || statusError}</p>{listenerError && statusError && <p>{statusError}</p>}</div>}

        {working && <section className="operation-panel" aria-label="Operation progress" aria-busy="true"><div className="operation-heading"><div><span className="activity-indicator" aria-hidden="true" /><strong>{operation === "verify" ? "Verifying original files" : "Installing / repairing files"}</strong><span className="phase">{progress ? progress.phase.replaceAll("_", " ") : "Preparing operation…"}</span></div><button type="button" onClick={() => void cancelOperation()} disabled={cancelling}>{cancelling ? "Cancelling…" : "Cancel"}</button></div><div className={`progress-track ${percent === null ? "indeterminate" : ""}`} role="progressbar" aria-label="Current operation" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent ?? undefined}><div style={percent === null ? undefined : { width: `${percent}%` }} /></div><div className="progress-stats"><span>{progress ? `${bytes(progress.completed_bytes)} / ${bytes(progress.total_bytes)} · ${number.format(progress.completed_files)} / ${number.format(progress.total_files)} files` : "Reading release metadata and preparing file checks."}</span><span>{percent === null ? "Working" : `${percent.toFixed(1)}%`}</span></div>{progress?.current_file && <p className="current-file" title={progress.current_file}>{progress.current_file}</p>}{cancelling && <p className="microcopy">Waiting for a safe file or download boundary. No game process will be stopped.</p>}</section>}

        {notice && <div ref={terminalFocus} tabIndex={-1} className={`notice ${notice.tone}`} role={notice.tone === "error" ? "alert" : "status"}><strong>{notice.title}</strong><p>{notice.detail}</p></div>}

        <section className="launch-bar" aria-label="Launch game"><div><strong>{status?.running ? "Arknights is running" : dirty ? "Your path changes are not saved" : status?.installed ? "Continue to Arknights" : "Set up your installation"}</strong><p>{status?.running ? "Your current session is left untouched." : dirty ? "Save or discard changes to use the selected installation." : status?.installed ? "Starts the original CN client using your selected Proton runtime." : "Choose an existing installation or install the game above."}</p></div><button type="button" className="launch-button" disabled={actionsLocked || !status?.installed || status.running || !settings?.proton_path} onClick={() => void launch()}>{pending === "launch" ? "Starting…" : status?.running ? "Game running" : "Launch game"}<Icon name="arrow" /></button></section>
      </main>

      <footer><p>Unofficial community launcher. Not affiliated with or endorsed by Hypergryph.<br />Game content and services remain subject to the publisher’s terms.</p><button type="button" className="text-button" onClick={() => void officialWebsite()}>Official Arknights website <span aria-hidden="true">↗</span></button></footer>
    </div>
  );
}
