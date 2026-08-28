import {
  Activity,
  ArrowLeft,
  ArrowRight,
  AudioLines,
  BookOpenText,
  Boxes,
  ChevronDown,
  Clapperboard,
  Clock3,
  Columns3,
  Cpu,
  FlaskConical,
  History,
  Info,
  Minus,
  Moon,
  MoreHorizontal,
  PanelLeft,
  Search,
  Settings,
  Square,
  Sun,
  UsersRound,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { FeatureState, HistoryItem, NavKey, SystemStatus, Theme } from "../types";
import { BrandLockup } from "./Brand";
import { AssistantLauncher, AssistantPane } from "./AssistantPane";

interface NavItem {
  key: NavKey;
  label: string;
  icon: LucideIcon;
}

const navGroups: Array<{ label: string; items: NavItem[] }> = [
  {
    label: "Create",
    items: [
      { key: "generate", label: "Generate", icon: AudioLines },
      { key: "video", label: "Video Studio", icon: Clapperboard },
      { key: "projects", label: "Projects", icon: BookOpenText },
    ],
  },
  {
    label: "Library",
    items: [
      { key: "voices", label: "Voices", icon: UsersRound },
      { key: "models", label: "Models", icon: Boxes },
      { key: "history", label: "History", icon: History },
    ],
  },
  {
    label: "Evaluate",
    items: [
      { key: "compare", label: "Compare", icon: Columns3 },
      { key: "benchmarks", label: "Benchmarks", icon: FlaskConical },
    ],
  },
];

const primaryNav = navGroups.flatMap((group) => group.items);
const mobilePrimaryKeys = new Set<NavKey>(["generate", "video", "projects", "history"]);

function WindowControls() {
  async function minimize() {
    await getCurrentWindow().minimize();
  }

  async function toggleMaximize() {
    await getCurrentWindow().toggleMaximize();
  }

  async function close() {
    await getCurrentWindow().close();
  }

  return (
    <div className="window-controls" aria-label="Window controls">
      <button type="button" aria-label="Minimize window" title="Minimize" onClick={() => void minimize()}>
        <Minus aria-hidden="true" size={13} strokeWidth={1.8} />
      </button>
      <button type="button" aria-label="Maximize window" title="Maximize or restore" onClick={() => void toggleMaximize()}>
        <Square aria-hidden="true" size={10} strokeWidth={1.8} />
      </button>
      <button className="window-close" type="button" aria-label="Close window" title="Close" onClick={() => void close()}>
        <X aria-hidden="true" size={13} strokeWidth={1.8} />
      </button>
    </div>
  );
}

const resizeDirections = ["North", "NorthEast", "East", "SouthEast", "South", "SouthWest", "West", "NorthWest"] as const;

function WindowResizeHandles() {
  function beginResize(direction: (typeof resizeDirections)[number]) {
    void getCurrentWindow().startResizeDragging(direction);
  }

  return (
    <div className="window-resize-handles" aria-hidden="true">
      {resizeDirections.map((direction) => (
        <div
          className={`window-resize-handle resize-${direction.toLowerCase()}`}
          key={direction}
          onPointerDown={() => void beginResize(direction)}
        />
      ))}
    </div>
  );
}

export function AppShell({
  current,
  onNavigate,
  theme,
  onToggleTheme,
  system,
  runtime,
  features,
  history = [],
  selectedHistoryId,
  onSelectHistory,
  assistantOpen,
  onAssistantOpenChange,
  onAssistantStudioChanged,
  children,
}: {
  current: NavKey;
  onNavigate: (key: NavKey) => void;
  theme: Theme;
  onToggleTheme: () => void;
  system: SystemStatus;
  runtime: "tauri" | "browser";
  features: Record<string, FeatureState>;
  history?: HistoryItem[];
  selectedHistoryId?: string;
  onSelectHistory?: (id: string) => void;
  assistantOpen: boolean;
  onAssistantOpenChange: (open: boolean) => void;
  onAssistantStudioChanged?: () => void;
  children: ReactNode;
}) {
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const [accountMenuOpen, setAccountMenuOpen] = useState(false);
  const [appMenu, setAppMenu] = useState<"file" | "edit" | "view" | "help">();
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [recentSearchOpen, setRecentSearchOpen] = useState(false);
  const [recentQuery, setRecentQuery] = useState("");
  const [backStack, setBackStack] = useState<NavKey[]>([]);
  const [forwardStack, setForwardStack] = useState<NavKey[]>([]);
  const contentRef = useRef<HTMLElement>(null);
  const accountRef = useRef<HTMLDivElement>(null);
  const appMenuRef = useRef<HTMLDivElement>(null);
  const accountButtonRef = useRef<HTMLButtonElement>(null);
  const availableVram = Math.max(0, system.vram_total_mb - system.vram_used_mb) / 1024;
  const settingsMode = current === "settings";
  const visibleHistory = history.filter((item) => `${item.title} ${item.text} ${item.voice} ${item.model_id}`.toLowerCase().includes(recentQuery.trim().toLowerCase())).slice(0, 24);

  function navigateTo(key: NavKey) {
    if (key === current) return;
    setBackStack((items) => [...items.slice(-24), current]);
    setForwardStack([]);
    setAppMenu(undefined);
    onNavigate(key);
  }

  function navigateBack() {
    const destination = backStack.at(-1);
    if (!destination) return;
    setBackStack((items) => items.slice(0, -1));
    setForwardStack((items) => [current, ...items].slice(0, 25));
    setAppMenu(undefined);
    onNavigate(destination);
  }

  function navigateForward() {
    const destination = forwardStack[0];
    if (!destination) return;
    setForwardStack((items) => items.slice(1));
    setBackStack((items) => [...items.slice(-24), current]);
    setAppMenu(undefined);
    onNavigate(destination);
  }

  useEffect(() => {
    if (contentRef.current) contentRef.current.scrollTop = 0;
    setAccountMenuOpen(false);
  }, [current]);

  useEffect(() => {
    function onPointerDown(event: PointerEvent) {
      if (accountMenuOpen && accountRef.current && !accountRef.current.contains(event.target as Node)) setAccountMenuOpen(false);
      if (appMenu && appMenuRef.current && !appMenuRef.current.contains(event.target as Node)) setAppMenu(undefined);
    }
    function onKeyDown(event: KeyboardEvent) {
      if ((event.ctrlKey || event.metaKey) && event.key === ",") {
        event.preventDefault();
        navigateTo("settings");
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "b") {
        event.preventDefault();
        setSidebarCollapsed((value) => !value);
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
        event.preventDefault();
        navigateTo("generate");
      }
      if (event.key === "Escape" && accountMenuOpen) {
        setAccountMenuOpen(false);
        accountButtonRef.current?.focus();
      }
      if (event.key === "Escape" && appMenu) setAppMenu(undefined);
    }
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [accountMenuOpen, appMenu, current, onNavigate]);

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
            navigateTo(item.key);
            setMobileMenuOpen(false);
          }
        }}
        type="button"
        title={disabled ? `${item.label} is not available in this build` : `${item.label}${state === "experimental" ? " (experimental)" : state === "beta" ? " (beta)" : ""}`}
        disabled={disabled}
        aria-current={current === item.key ? "page" : undefined}
      >
        <Icon aria-hidden="true" size={16} strokeWidth={1.75} />
        <span>{item.label}</span>
      </button>
    );
  };

  return (
    <div className={`app-shell ${settingsMode ? "is-settings-mode" : ""} ${sidebarCollapsed ? "is-sidebar-collapsed" : ""} ${assistantOpen ? "is-assistant-open" : ""}`}>
        <header className="app-topbar" data-tauri-drag-region>
          <div className="topbar-brand-cell" ref={appMenuRef}>
            <button className="topbar-command" type="button" aria-label={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"} title={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"} onClick={() => setSidebarCollapsed((value) => !value)}><PanelLeft aria-hidden="true" size={14} /></button>
            <button className="topbar-command" type="button" aria-label="Go back" title="Back" disabled={!backStack.length} onClick={navigateBack}><ArrowLeft aria-hidden="true" size={15} /></button>
            <button className="topbar-command" type="button" aria-label="Go forward" title="Forward" disabled={!forwardStack.length} onClick={navigateForward}><ArrowRight aria-hidden="true" size={15} /></button>
            <div className="desktop-menu-set">
              {(["file", "edit", "view", "help"] as const).map((menu) => <button className={appMenu === menu ? "is-open" : ""} key={menu} type="button" aria-haspopup="menu" aria-expanded={appMenu === menu} onClick={() => setAppMenu((open) => open === menu ? undefined : menu)}>{menu[0].toUpperCase() + menu.slice(1)}</button>)}
            </div>
            {appMenu ? <div className={`topbar-menu-popover menu-${appMenu}`} role="menu" aria-label={`${appMenu} menu`}>
              {appMenu === "file" ? <><button role="menuitem" type="button" onClick={() => navigateTo("generate")}><span>New generation</span><kbd>Ctrl+N</kbd></button><button role="menuitem" type="button" onClick={() => navigateTo("video")}><span>New video project</span></button><button role="menuitem" type="button" onClick={() => navigateTo("projects")}><span>Projects</span></button><div className="account-menu-separator" /><button role="menuitem" type="button" onClick={() => navigateTo("settings")}><span>Settings</span><kbd>Ctrl+,</kbd></button></> : null}
              {appMenu === "edit" ? <><button role="menuitem" type="button" onClick={() => { setAppMenu(undefined); document.querySelector<HTMLElement>("main textarea, main input")?.focus(); }}><span>Focus editor</span></button><button role="menuitem" type="button" onClick={() => { setAppMenu(undefined); document.execCommand("selectAll"); }}><span>Select all</span><kbd>Ctrl+A</kbd></button><button role="menuitem" type="button" onClick={() => { setAppMenu(undefined); document.execCommand("copy"); }}><span>Copy</span><kbd>Ctrl+C</kbd></button></> : null}
              {appMenu === "view" ? <><button role="menuitem" type="button" onClick={() => { setSidebarCollapsed((value) => !value); setAppMenu(undefined); }}><span>{sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}</span><kbd>Ctrl+B</kbd></button><button role="menuitem" type="button" onClick={() => navigateTo("history")}><span>History</span></button><div className="account-menu-separator" /><button role="menuitem" type="button" onClick={() => { onToggleTheme(); setAppMenu(undefined); }}><span>{theme === "dark" ? "Light appearance" : "Dark appearance"}</span></button></> : null}
              {appMenu === "help" ? <><button role="menuitem" type="button" onClick={() => navigateTo("about")}><span>About soundAr</span></button><button role="menuitem" type="button" onClick={() => navigateTo("settings")}><span>Runtime settings</span></button></> : null}
            </div> : null}
          </div>
          <div className="topbar-drag-region" data-tauri-drag-region aria-hidden="true" />
          <div className="topbar-status" data-tauri-drag-region>
            <span className="runtime-indicator" title={`${system.gpu_name} · ${availableVram.toFixed(1)} GB available`}>
              <Activity aria-hidden="true" size={13} />
              {system.cuda_available ? "CUDA ready" : "CPU runtime"}
            </span>
            {runtime === "browser" ? <em>Preview</em> : null}
          </div>
          {runtime === "tauri" ? <WindowControls /> : null}
        </header>

        {!settingsMode ? (
          <aside className="sidebar">
            <div className="sidebar-product-row"><BrandLockup /><button className="icon-button" type="button" aria-label="Search generation history" title="Search generation history" aria-expanded={recentSearchOpen} onClick={() => setRecentSearchOpen((open) => !open)}><Search aria-hidden="true" size={14} /></button></div>
            <div className="sidebar-scroll">
              {recentSearchOpen ? <label className="sidebar-history-search"><Search aria-hidden="true" size={13} /><input autoFocus aria-label="Search generation history" value={recentQuery} onChange={(event) => setRecentQuery(event.target.value)} placeholder="Search history" /></label> : null}
              {navGroups.map((group) => (
                <div className="nav-group" key={group.label}>
                  <span className="nav-section-label">{group.label}</span>
                  <nav aria-label={`${group.label} navigation`}>{group.items.map(renderNavItem)}</nav>
                </div>
              ))}
              {history.length ? <div className="nav-group recent-work-group"><span className="nav-section-label">Recent</span><nav aria-label="Recent generations">{visibleHistory.map((item) => <button className={`recent-work-item ${current === "history" && selectedHistoryId === item.id ? "is-active" : ""}`} key={item.id} type="button" title={item.title || item.text} aria-current={current === "history" && selectedHistoryId === item.id ? "page" : undefined} onClick={() => { onSelectHistory?.(item.id); navigateTo("history"); }}><span>{item.title || item.text.slice(0, 48) || "Untitled generation"}</span><small>{item.generation_kind === "music" ? "Music" : item.voice || "Voice"}</small></button>)}{!visibleHistory.length ? <small className="sidebar-history-empty">No matching generations</small> : null}</nav></div> : null}
            </div>
            <div className="sidebar-footer" ref={accountRef}>
              {accountMenuOpen ? (
                <div className="account-menu" role="menu" aria-label="Application menu">
                  <div className="account-identity">
                    <span><strong>soundAr</strong><small>Local voice studio</small></span>
                  </div>
                  <div className="account-runtime-row">
                    <Cpu aria-hidden="true" size={15} />
                    <span><strong>{system.cuda_available ? "GPU runtime ready" : "CPU runtime"}</strong><small>{availableVram.toFixed(1)} GB available · local only</small></span>
                  </div>
                  <div className="account-menu-separator" />
                  <button role="menuitem" type="button" onClick={onToggleTheme}>
                    {theme === "dark" ? <Sun aria-hidden="true" size={16} /> : <Moon aria-hidden="true" size={16} />}
                    <span>{theme === "dark" ? "Light appearance" : "Dark appearance"}</span>
                  </button>
                  <button role="menuitem" type="button" onClick={() => navigateTo("settings")}>
                    <Settings aria-hidden="true" size={16} />
                    <span>Settings</span><kbd>Ctrl+,</kbd>
                  </button>
                  <button role="menuitem" type="button" onClick={() => navigateTo("about")}>
                    <Info aria-hidden="true" size={16} />
                    <span>About soundAr</span>
                  </button>
                </div>
              ) : null}
              <button
                ref={accountButtonRef}
                className={`account-trigger ${current === "about" ? "is-active" : ""}`}
                type="button"
                aria-haspopup="menu"
                aria-expanded={accountMenuOpen}
                onClick={() => setAccountMenuOpen((open) => !open)}
              >
                <span><strong>Local studio</strong><small>{system.cuda_available ? "GPU ready" : "CPU mode"}</small></span>
                <ChevronDown aria-hidden="true" size={14} />
              </button>
            </div>
          </aside>
        ) : null}

        <main className="app-content" ref={contentRef}>{children}</main>

        <AssistantPane open={assistantOpen} onClose={() => onAssistantOpenChange(false)} onStudioChanged={onAssistantStudioChanged} />
        {!assistantOpen ? <AssistantLauncher onClick={() => onAssistantOpenChange(true)} /> : null}

        {runtime === "tauri" ? <WindowResizeHandles /> : null}

        {mobileMenuOpen ? (
          <div className="mobile-more-menu" id="mobile-more-menu">
            {primaryNav.filter((item) => !mobilePrimaryKeys.has(item.key)).map(renderNavItem)}
            <button className={`nav-item ${current === "settings" ? "is-active" : ""}`} onClick={() => { navigateTo("settings"); setMobileMenuOpen(false); }} type="button">
              <Settings aria-hidden="true" size={17} />
              <span>Settings</span>
            </button>
            <button className={`nav-item ${current === "about" ? "is-active" : ""}`} onClick={() => { navigateTo("about"); setMobileMenuOpen(false); }} type="button">
              <Info aria-hidden="true" size={17} />
              <span>About</span>
            </button>
          </div>
        ) : null}
        {!settingsMode ? (
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
        ) : null}
    </div>
  );
}
