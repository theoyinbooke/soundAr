import {
  Activity,
  AudioLines,
  Boxes,
  Clock3,
  Columns3,
  FlaskConical,
  History,
  Info,
  Mic2,
  Moon,
  Settings,
  Sun,
  UsersRound,
  type LucideIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import type { NavKey, SystemStatus, Theme } from "../types";
import { BrandLockup } from "./Brand";

interface NavItem {
  key: NavKey;
  label: string;
  icon: LucideIcon;
}

const primaryNav: NavItem[] = [
  { key: "generate", label: "Generate", icon: AudioLines },
  { key: "voices", label: "Voices", icon: UsersRound },
  { key: "models", label: "Models", icon: Boxes },
  { key: "live", label: "Live", icon: Mic2 },
  { key: "compare", label: "Compare", icon: Columns3 },
  { key: "benchmarks", label: "Benchmarks", icon: FlaskConical },
  { key: "history", label: "History", icon: History },
];

export function AppShell({
  current,
  onNavigate,
  theme,
  onToggleTheme,
  system,
  runtime,
  children,
}: {
  current: NavKey;
  onNavigate: (key: NavKey) => void;
  theme: Theme;
  onToggleTheme: () => void;
  system: SystemStatus;
  runtime: "tauri" | "browser";
  children: ReactNode;
}) {
  const availableVram = Math.max(0, system.vram_total_mb - system.vram_used_mb) / 1024;

  const renderNavItem = (item: NavItem) => {
    const Icon = item.icon;
    return (
      <button
        className={`nav-item ${current === item.key ? "is-active" : ""}`}
        key={item.key}
        onClick={() => onNavigate(item.key)}
        type="button"
        title={item.label}
        aria-current={current === item.key ? "page" : undefined}
      >
        <Icon aria-hidden="true" size={17} strokeWidth={1.7} />
        <span>{item.label}</span>
      </button>
    );
  };

  return (
    <div className="app-shell">
      <header className="app-topbar">
        <BrandLockup className="brand-lockup-topbar" tagline="Local voice studio" />
        <div className="topbar-status">
          <Activity aria-hidden="true" size={14} />
          <span>
            {system.cuda_available ? "CUDA ready" : "CPU runtime"} · {availableVram.toFixed(1)} GB free
          </span>
          {runtime === "browser" ? <em>Browser preview</em> : null}
        </div>
      </header>

      <aside className="sidebar">
        <span className="nav-section-label">Workspace</span>
        <nav aria-label="Primary navigation">{primaryNav.map(renderNavItem)}</nav>
        <div className="sidebar-footer">
          <button
            className={`nav-item ${current === "settings" ? "is-active" : ""}`}
            onClick={() => onNavigate("settings")}
            type="button"
            title="Settings"
          >
            <Settings aria-hidden="true" size={17} strokeWidth={1.7} />
            <span>Settings</span>
          </button>
          <button
            className={`nav-item ${current === "about" ? "is-active" : ""}`}
            onClick={() => onNavigate("about")}
            type="button"
            title="About soundAr"
          >
            <Info aria-hidden="true" size={17} strokeWidth={1.7} />
            <span>About</span>
          </button>
          <button className="theme-button" onClick={onToggleTheme} type="button" title="Toggle color theme">
            {theme === "dark" ? <Sun aria-hidden="true" size={16} /> : <Moon aria-hidden="true" size={16} />}
            <span>{theme === "dark" ? "Cream light" : "Dark mode"}</span>
          </button>
          <div className="runtime-footnote">
            <Clock3 aria-hidden="true" size={13} />
            <span>Local only</span>
          </div>
        </div>
      </aside>

      <main className="app-content">{children}</main>

      <nav className="mobile-nav" aria-label="Mobile navigation">
        {primaryNav.map(renderNavItem)}
        <button
          className={`nav-item ${current === "settings" ? "is-active" : ""}`}
          onClick={() => onNavigate("settings")}
          type="button"
          title="Settings"
        >
          <Settings aria-hidden="true" size={17} />
          <span>Settings</span>
        </button>
        <button
          className={`nav-item ${current === "about" ? "is-active" : ""}`}
          onClick={() => onNavigate("about")}
          type="button"
          title="About soundAr"
        >
          <Info aria-hidden="true" size={17} />
          <span>About</span>
        </button>
      </nav>
    </div>
  );
}
