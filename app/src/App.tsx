import { useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { AppShell } from "./components/AppShell";
import { RuntimeSetupNotice } from "./components/RuntimeSetupNotice";
import { UpdateNotice } from "./components/UpdateNotice";
import { LoadingView, RuntimeFailureView } from "./components/ui";
import { listHistory, loadBootstrapState, saveApplicationSetting } from "./lib/bridge";
import type { ApplicationSettings, BootstrapState, HistoryItem, NavKey, ProjectRecord, Theme, UpdateCheckStatus } from "./types";
import { BenchmarksView } from "./views/BenchmarksView";
import { GenerateView } from "./views/GenerateView";
import { ModelsView } from "./views/ModelsView";
import { AboutView, CompareView, HistoryView, SettingsView } from "./views/SecondaryViews";
import { VoicesView } from "./views/VoicesView";
import { ProjectsView } from "./views/ProjectsView";

export default function App() {
  const [settings, setSettings] = useState<ApplicationSettings>({ theme: "light", dense_tables: true, reduced_motion: false });
  const [current, setCurrent] = useState<NavKey>("generate");
  const [bootstrap, setBootstrap] = useState<BootstrapState>();
  const [bootstrapError, setBootstrapError] = useState<string>();
  const [runtimeNotice, setRuntimeNotice] = useState<string>();
  const [bootstrapAttempt, setBootstrapAttempt] = useState(0);
  const [loading, setLoading] = useState(true);
  const [voices, setVoices] = useState<BootstrapState["voices"]>([]);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [selectedHistoryId, setSelectedHistoryId] = useState<string>();
  const [projects, setProjects] = useState<ProjectRecord[]>([]);
  const [availableUpdate, setAvailableUpdate] = useState<Update>();
  const [updateCheck, setUpdateCheck] = useState<UpdateCheckStatus>({ phase: "idle" });
  const [preferredVoiceId, setPreferredVoiceId] = useState<string>();

  useEffect(() => {
    document.documentElement.dataset.theme = settings.theme;
    document.documentElement.dataset.density = settings.dense_tables ? "dense" : "comfortable";
    document.documentElement.dataset.motion = settings.reduced_motion ? "reduced" : "full";
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", settings.theme === "dark" ? "#1f1f1f" : "#ffffff");
  }, [settings]);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setBootstrapError(undefined);
    loadBootstrapState()
      .then(async (state) => ({ state, history: await listHistory() }))
      .then(({ state, history: loadedHistory }) => {
        if (!active) return;
        setBootstrap(state);
        setSettings(state.settings);
        setVoices(state.voices);
        setProjects(state.projects);
        setHistory(loadedHistory);
        setSelectedHistoryId((selected) => selected && loadedHistory.some((item) => item.id === selected) ? selected : loadedHistory[0]?.id);
      })
      .catch((caught) => {
        if (!active) return;
        setBootstrap(undefined);
        setBootstrapError(caught instanceof Error ? caught.message : String(caught));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => { active = false; };
  }, [bootstrapAttempt]);

  useEffect(() => {
    if (bootstrap?.runtime !== "tauri") return;
    let active = true;
    async function checkForUpdate() {
      try {
        const update = await check({ timeout: 15_000 });
        if (active && update) setAvailableUpdate(update);
      } catch {
        // Update checks are best-effort and should not interrupt local work.
      }
    }
    const initial = window.setTimeout(checkForUpdate, 1_500);
    const interval = window.setInterval(checkForUpdate, 6 * 60 * 60 * 1000);
    return () => {
      active = false;
      window.clearTimeout(initial);
      window.clearInterval(interval);
    };
  }, [bootstrap?.runtime]);

  function renderView(state: BootstrapState) {
    switch (current) {
      case "generate": return <GenerateView bootstrap={state} voices={voices} onVoicesChange={setVoices} preferredVoiceId={preferredVoiceId} onOpenModels={() => setCurrent("models")} onGenerated={(item) => setHistory((items) => [item, ...items.filter((existing) => existing.id !== item.id)])} />;
      case "projects": return <ProjectsView bootstrap={state} projects={projects} voices={voices} onChange={setProjects} onGenerated={(item) => setHistory((items) => [item, ...items.filter((existing) => existing.id !== item.id)])} />;
      case "voices": return <VoicesView bootstrap={state} voices={voices} onChange={setVoices} onGenerated={(item) => setHistory((items) => [item, ...items.filter((existing) => existing.id !== item.id)])} onUseVoice={(id) => { setPreferredVoiceId(id); setCurrent("generate"); }} />;
      case "models": return <ModelsView bootstrap={state} onChanged={refreshBootstrap} />;
      case "compare": return <CompareView bootstrap={state} onGenerated={(item) => setHistory((items) => [item, ...items.filter((existing) => existing.id !== item.id)])} />;
      case "benchmarks": return <BenchmarksView bootstrap={state} onGenerated={(item) => setHistory((items) => [item, ...items.filter((existing) => existing.id !== item.id)])} />;
      case "history": return <HistoryView history={history} onChange={setHistory} selectedId={selectedHistoryId} />;
      case "settings": return <SettingsView bootstrap={state} settings={settings} onSetting={updateSetting} updateCheck={updateCheck} onCheckForUpdates={checkForUpdatesNow} onBack={() => setCurrent("generate")} />;
      case "about": return <AboutView bootstrap={state} updateCheck={updateCheck} onCheckForUpdates={checkForUpdatesNow} />;
    }
  }

  if (loading) return <LoadingView />;
  if (bootstrapError || !bootstrap) {
    return <RuntimeFailureView error={bootstrapError ?? "The local runtime returned no application state."} onRetry={() => setBootstrapAttempt((attempt) => attempt + 1)} />;
  }

  async function refreshBootstrap() {
    try {
      const state = await loadBootstrapState();
      setBootstrap(state);
      setSettings(state.settings);
      setRuntimeNotice(undefined);
    } catch (caught) {
      setRuntimeNotice(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function updateSetting<K extends keyof ApplicationSettings>(key: K, value: ApplicationSettings[K]) {
    setSettings((currentSettings) => ({ ...currentSettings, [key]: value }));
    try {
      const saved = await saveApplicationSetting(key, value);
      setSettings(saved);
    } catch (caught) {
      setRuntimeNotice(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function checkForUpdatesNow() {
    if (bootstrap?.runtime !== "tauri") {
      setUpdateCheck({ phase: "unavailable", message: "Update checks are available in the installed desktop app." });
      return;
    }
    setUpdateCheck({ phase: "checking", message: "Checking the signed release feed..." });
    try {
      const update = await check({ timeout: 15_000 });
      if (update) {
        setAvailableUpdate(update);
        setUpdateCheck({ phase: "available", message: `soundAr ${update.version} is available.` });
      } else {
        setUpdateCheck({ phase: "current", message: `soundAr ${__APP_VERSION__} is up to date.` });
      }
    } catch (caught) {
      setUpdateCheck({
        phase: "error",
        message: caught instanceof Error ? `Update check failed: ${caught.message}` : `Update check failed: ${String(caught)}`,
      });
    }
  }

  return (
    <AppShell current={current} onNavigate={setCurrent} theme={settings.theme} onToggleTheme={() => void updateSetting("theme", settings.theme === "dark" ? "light" : "dark")} system={bootstrap.system} runtime={bootstrap.runtime} features={bootstrap.features} history={history} selectedHistoryId={selectedHistoryId} onSelectHistory={setSelectedHistoryId}>
      {runtimeNotice ? <div className="runtime-warning">Local operation failed: {runtimeNotice}</div> : null}
      {availableUpdate ? <UpdateNotice update={availableUpdate} installKind={bootstrap.install_kind} onDismiss={() => { void availableUpdate.close(); setAvailableUpdate(undefined); }} /> : null}
      {bootstrap.runtime === "tauri" && !bootstrap.system.python_ready ? <RuntimeSetupNotice onReady={refreshBootstrap} /> : null}
      {renderView(bootstrap)}
    </AppShell>
  );
}
