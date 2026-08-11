import { useEffect, useState } from "react";
import { AppShell } from "./components/AppShell";
import { LoadingView } from "./components/ui";
import { fallbackBootstrap, seedBenchmarks } from "./data";
import { loadBootstrapState } from "./lib/bridge";
import type { BenchmarkResult, BootstrapState, HistoryItem, NavKey, Theme, VoiceProfile } from "./types";
import { BenchmarksView } from "./views/BenchmarksView";
import { GenerateView } from "./views/GenerateView";
import { ModelsView } from "./views/ModelsView";
import { CompareView, HistoryView, LiveView, SettingsView } from "./views/SecondaryViews";
import { VoicesView } from "./views/VoicesView";

const savedTheme = localStorage.getItem("soundar.theme");
const initialTheme: Theme = savedTheme === "light" ? "light" : "dark";

export default function App() {
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [current, setCurrent] = useState<NavKey>("generate");
  const [bootstrap, setBootstrap] = useState<BootstrapState>();
  const [error, setError] = useState<string>();
  const [voices, setVoices] = useState<VoiceProfile[]>(() => {
    try { return JSON.parse(localStorage.getItem("soundar.voices") ?? "null") ?? fallbackBootstrap.voices; }
    catch { return fallbackBootstrap.voices; }
  });
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [benchmarks, setBenchmarks] = useState<BenchmarkResult[]>(seedBenchmarks);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("soundar.theme", theme);
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", theme === "dark" ? "#111412" : "#f4efe3");
  }, [theme]);

  useEffect(() => {
    loadBootstrapState()
      .then((state) => {
        setBootstrap(state);
        if (!localStorage.getItem("soundar.voices") && state.voices.length) setVoices(state.voices);
      })
      .catch((caught) => {
        setError(caught instanceof Error ? caught.message : String(caught));
        setBootstrap(fallbackBootstrap);
      });
  }, []);

  useEffect(() => { localStorage.setItem("soundar.voices", JSON.stringify(voices)); }, [voices]);

  function renderView(state: BootstrapState) {
    switch (current) {
      case "generate": return <GenerateView bootstrap={state} voices={voices} onGenerated={(item) => setHistory((items) => [item, ...items])} />;
      case "voices": return <VoicesView voices={voices} onChange={setVoices} />;
      case "models": return <ModelsView bootstrap={state} />;
      case "live": return <LiveView bootstrap={state} />;
      case "compare": return <CompareView bootstrap={state} />;
      case "benchmarks": return <BenchmarksView bootstrap={state} results={benchmarks} onChange={setBenchmarks} />;
      case "history": return <HistoryView history={history} />;
      case "settings": return <SettingsView bootstrap={state} theme={theme} onTheme={setTheme} />;
    }
  }

  if (!bootstrap) return <LoadingView />;

  return (
    <AppShell current={current} onNavigate={setCurrent} theme={theme} onToggleTheme={() => setTheme((value) => value === "dark" ? "light" : "dark")} system={bootstrap.system} runtime={bootstrap.runtime}>
      {error ? <div className="runtime-warning">Desktop runtime unavailable. Showing the browser preview: {error}</div> : null}
      {renderView(bootstrap)}
    </AppShell>
  );
}
