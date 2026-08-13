import {
  Activity,
  AudioLines,
  BookOpenText,
  Boxes,
  Clock3,
  Captions,
  Columns3,
  FlaskConical,
  History,
  Info,
  Mic2,
  Moon,
  MoreHorizontal,
  Settings,
  Sun,
  UsersRound,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { FeatureState, NavKey, SystemStatus, Theme } from "../types";
import { BrandLockup } from "./Brand";

interface NavItem {
  key: NavKey;
  label: string;
  icon: LucideIcon;
}

const primaryNav: NavItem[] = [
  { key: "generate", label: "Generate", icon: AudioLines },
  { key: "projects", label: "Projects", icon: BookOpenText },
  { key: "transcribe", label: "Transcribe", icon: Captions },
  { key: "voices", label: "Voices", icon: UsersRound },
  { key: "models", label: "Models", icon: Boxes },
  { key: "live", label: "Live", icon: Mic2 },
  { key: "compare", label: "Compare", icon: Columns3 },
  { key: "benchmarks", label: "Benchmarks", icon: FlaskConical },
  { key: "history", label: "History", icon: History },
];

const mobilePrimaryKeys = new Set<NavKey>(["generate", "projects", "transcribe", "voices"]);

export function AppShell({
  current,
  onNavigate,
  theme,
  onToggleTheme,
  system,
  runtime,
  features,
  children,
}: {
  current: NavKey;
  onNavigate: (key: NavKey) => void;
  theme: Theme;
  onToggleTheme: () => void;
  system: SystemStatus;
  runtime: "tauri" | "browser";
  features: Record<string, FeatureState>;
  children: ReactNode;
}) {
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const contentRef = useRef<HTMLElement>(null);
  const availableVram = Math.max(0, system.vram_total_mb - system.vram_used_mb) / 1024;

  useEffect(() => {
    if (contentRef.current) contentRef.current.scrollTop = 0;
  }, [current]);

  const renderNavItem = (item: NavItem) => {
    const Icon = item.icon;
    const state = features[item.key] ?? "stable";
    const disabled = state === "disabled";
    return (
      <button
        className={`nav-item ${current === item.key ? "is-active" : ""} ${disabled ? "is-disabled" : ""}`}
        key={item.key}
        aria-label={item.label}
        onClick={() => {
          if (!disabled) {
            onNavigate(item.key);
            setMobileMenuOpen(false);
          }
        }}
        type="button"
        title={disabled ? `${item.label} is not available in this build` : `${item.label}${state === "experimental" ? " (experimental)" : state === "beta" ? " (beta)" : ""}`}
        disabled={disabled}
        aria-current={current === item.key ? "page" : undefined}
      >
        <Icon aria-hidden="true" size={17} strokeWidth={1.7} />
        <span>{item.label}</span>
        {state !== "stable" ? <em aria-hidden="true" className={`feature-state feature-${state}`}>{state === "experimental" ? "Labs" : state}</em> : null}
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
          <button className="topbar-theme" onClick={onToggleTheme} type="button" title={theme === "dark" ? "Cream light" : "Dark mode"} aria-label={theme === "dark" ? "Cream light" : "Dark mode"}>
            {theme === "dark" ? <Sun aria-hidden="true" size={15} /> : <Moon aria-hidden="true" size={15} />}
          </button>
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
            aria-label="About"
            onClick={() => onNavigate("about")}
            type="button"
            title="About soundAr"
          >
            <Info aria-hidden="true" size={17} strokeWidth={1.7} />
            <span>About</span>
          </button>
          <button className="theme-button" aria-label={theme === "dark" ? "Cream light" : "Dark mode"} onClick={onToggleTheme} type="button" title={theme === "dark" ? "Cream light" : "Dark mode"}>
            {theme === "dark" ? <Sun aria-hidden="true" size={16} /> : <Moon aria-hidden="true" size={16} />}
            <span>{theme === "dark" ? "Cream light" : "Dark mode"}</span>
          </button>
          <div className="runtime-footnote">
            <Clock3 aria-hidden="true" size={13} />
            <span>Local only</span>
          </div>
        </div>
      </aside>

      <main className="app-content" ref={contentRef}>{children}</main>

      {mobileMenuOpen ? (
        <div className="mobile-more-menu" id="mobile-more-menu">
          {primaryNav.filter((item) => !mobilePrimaryKeys.has(item.key)).map(renderNavItem)}
          <button className={`nav-item ${current === "settings" ? "is-active" : ""}`} onClick={() => { onNavigate("settings"); setMobileMenuOpen(false); }} type="button">
            <Settings aria-hidden="true" size={17} />
            <span>Settings</span>
          </button>
          <button className={`nav-item ${current === "about" ? "is-active" : ""}`} onClick={() => { onNavigate("about"); setMobileMenuOpen(false); }} type="button">
            <Info aria-hidden="true" size={17} />
            <span>About</span>
          </button>
        </div>
      ) : null}
      <nav className="mobile-nav" aria-label="Mobile navigation">
        {primaryNav.filter((item) => mobilePrimaryKeys.has(item.key)).map(renderNavItem)}
        <button
          className={`nav-item ${!mobilePrimaryKeys.has(current) ? "is-active" : ""}`}
          aria-expanded={mobileMenuOpen}
          aria-controls="mobile-more-menu"
          aria-label="More navigation"
          onClick={() => setMobileMenuOpen((open) => !open)}
          type="button"
          title="More navigation"
        >
          <MoreHorizontal aria-hidden="true" size={18} />
          <span>More</span>
        </button>
      </nav>
    </div>
  );
}
