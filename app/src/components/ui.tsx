import { AlertTriangle, Check, ChevronDown, Ellipsis, LoaderCircle, Pause, Play, RefreshCw } from "lucide-react";
import { useContext, useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { BrandMark } from "./Brand";
import { PageToolbarTargetContext } from "./PageToolbarContext";

export interface DropdownOption {
  value: string;
  label: string;
}

export function PageHeader({
  title,
  subtitle,
  actions,
}: {
  title: string;
  subtitle: string;
  actions?: ReactNode;
}) {
  const toolbarTarget = useContext(PageToolbarTargetContext);
  const content = (
    <div className="page-header-content">
      <div className="page-heading">
        <h1>{title}</h1>
        <p>{subtitle}</p>
      </div>
      {actions ? <div className="page-actions">{actions}</div> : null}
    </div>
  );
  return toolbarTarget
    ? createPortal(content, toolbarTarget)
    : <header className="page-header">{content}</header>;
}

export function Panel({
  children,
  className = "",
  ariaLabel,
}: {
  children: ReactNode;
  className?: string;
  ariaLabel?: string;
}) {
  return (
    <section className={`panel ${className}`} aria-label={ariaLabel}>
      {children}
    </section>
  );
}

export function StatusText({
  tone = "muted",
  children,
}: {
  tone?: "success" | "warning" | "danger" | "muted";
  children: ReactNode;
}) {
  return <span className={`status-text status-${tone}`}>{children}</span>;
}

export interface RowAction {
  label: string;
  icon?: ReactNode;
  onSelect: () => void | Promise<void>;
  disabled?: boolean;
  danger?: boolean;
}

export function RowActionMenu({
  label,
  actions,
}: {
  label: string;
  actions: RowAction[];
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const menuId = useId();

  useEffect(() => {
    if (!open) return;
    const focusFrame = window.requestAnimationFrame(() => {
      menu.current?.querySelector<HTMLButtonElement>('button[role="menuitem"]:not(:disabled)')?.focus();
    });
    const closeOutside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!menu.current?.contains(target) && !trigger.current?.contains(target)) setOpen(false);
    };
    const closeOnViewportChange = () => setOpen(false);
    const closeOnKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setOpen(false);
        trigger.current?.focus();
      }
    };
    document.addEventListener("pointerdown", closeOutside);
    window.addEventListener("resize", closeOnViewportChange);
    window.addEventListener("scroll", closeOnViewportChange, true);
    document.addEventListener("keydown", closeOnKey);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      document.removeEventListener("pointerdown", closeOutside);
      window.removeEventListener("resize", closeOnViewportChange);
      window.removeEventListener("scroll", closeOnViewportChange, true);
      document.removeEventListener("keydown", closeOnKey);
    };
  }, [open]);

  function toggle() {
    if (open) {
      setOpen(false);
      return;
    }
    const anchor = trigger.current?.getBoundingClientRect();
    if (!anchor) return;
    const width = 184;
    const height = Math.min(264, actions.length * 31 + 8);
    const workspace = trigger.current?.closest<HTMLElement>(".app-content")?.getBoundingClientRect();
    const topEdge = (workspace?.top ?? 0) + 8;
    const bottomEdge = (workspace?.bottom ?? window.innerHeight) - 8;
    const top = anchor.bottom + height + 4 <= bottomEdge
      ? anchor.bottom + 4
      : Math.max(topEdge, anchor.top - height - 4);
    const left = Math.max(8, Math.min(window.innerWidth - width - 8, anchor.right - width));
    setPosition({ top, left });
    setOpen(true);
  }

  function moveFocus(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]:not(:disabled)')];
    if (!items.length) return;
    event.preventDefault();
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "Home"
      ? 0
      : event.key === "End"
        ? items.length - 1
        : event.key === "ArrowDown"
          ? (current + 1 + items.length) % items.length
          : (current - 1 + items.length) % items.length;
    items[next]?.focus();
  }

  return (
    <>
      <button
        ref={trigger}
        className="icon-button row-action-trigger"
        type="button"
        title={label}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={(event) => {
          event.stopPropagation();
          toggle();
        }}
      >
        <Ellipsis aria-hidden="true" size={14} />
      </button>
      {open ? createPortal(
        <div
          ref={menu}
          className="row-action-popover"
          id={menuId}
          role="menu"
          aria-label={label}
          style={position}
          onKeyDown={moveFocus}
          onPointerDown={(event) => event.stopPropagation()}
        >
          {actions.map((action) => (
            <button
              className={action.danger ? "danger-button" : undefined}
              key={action.label}
              role="menuitem"
              type="button"
              disabled={action.disabled}
              onClick={(event) => {
                event.stopPropagation();
                setOpen(false);
                trigger.current?.focus();
                void action.onSelect();
              }}
            >
              {action.icon}
              <span>{action.label}</span>
            </button>
          ))}
        </div>,
        document.body,
      ) : null}
    </>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {options.map((option) => (
        <button
          className={value === option.value ? "is-active" : ""}
          key={option.value}
          onClick={() => onChange(option.value)}
          type="button"
          aria-pressed={value === option.value}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function SelectField({
  label,
  value,
  onChange,
  options,
  status,
  disabled = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  status?: string;
  disabled?: boolean;
}) {
  return (
    <div className="field field-select">
      <span className="field-label">{label}</span>
      <Dropdown ariaLabel={label} value={value} onChange={onChange} options={options} status={status} disabled={disabled} />
    </div>
  );
}

export function Dropdown({
  ariaLabel,
  value,
  onChange,
  options,
  status,
  disabled = false,
}: {
  ariaLabel: string;
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  status?: string;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [opensUp, setOpensUp] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const selected = options.find((option) => option.value === value) ?? options[0];

  useEffect(() => {
    function close(event: PointerEvent) {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    }
    function escape(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", escape);
    };
  }, []);

  function toggle() {
    if (disabled) return;
    if (!open && root.current) {
      const trigger = root.current.getBoundingClientRect();
      const workspace = root.current.closest<HTMLElement>(".app-content")?.getBoundingClientRect();
      const lowerEdge = workspace?.bottom ?? window.innerHeight;
      const upperEdge = workspace?.top ?? 0;
      const menuHeight = Math.min(214, options.length * 30 + 10);
      const spaceBelow = lowerEdge - trigger.bottom - 4;
      const spaceAbove = trigger.top - upperEdge - 4;
      setOpensUp(spaceBelow < menuHeight && spaceAbove > spaceBelow);
    }
    setOpen((current) => !current);
  }

  return (
    <div className={`dropdown ${open ? "is-open" : ""} ${opensUp ? "opens-up" : ""}`} ref={root}>
      <button
        className="dropdown-trigger"
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-expanded={open}
        disabled={disabled}
        aria-controls={`${ariaLabel.replace(/\s+/g, "-").toLowerCase()}-options`}
        onClick={toggle}
      >
        <span className="dropdown-value">{selected?.label ?? "Select"}</span>
        {status ? <StatusText tone="success">{status}</StatusText> : null}
        <ChevronDown aria-hidden="true" size={13} />
      </button>
      {open ? (
        <div className="dropdown-menu" id={`${ariaLabel.replace(/\s+/g, "-").toLowerCase()}-options`} role="listbox" aria-label={`${ariaLabel} options`}>
          {options.map((option) => (
            <button
              className={`dropdown-option ${option.value === value ? "is-selected" : ""}`}
              key={option.value}
              type="button"
              role="option"
              aria-selected={option.value === value}
              onClick={() => { onChange(option.value); setOpen(false); }}
            >
              <span>{option.label}</span>
              {option.value === value ? <Check aria-hidden="true" size={12} /> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

export function CompactField({
  label,
  children,
  className = "",
}: {
  label: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={`field compact-field ${className}`}>
      <span className="field-label">{label}</span>
      {children}
    </div>
  );
}

export function LoadingView() {
  return (
    <div className="loading-view" role="status">
      <BrandMark className="loading-brand" />
      <div className="loading-status"><LoaderCircle className="spin" aria-hidden="true" size={14} /><span>Reading the local runtime...</span></div>
    </div>
  );
}

export function RuntimeFailureView({ error, onRetry }: { error: string; onRetry: () => void }) {
  return (
    <main className="runtime-failure-view">
      <BrandMark className="loading-brand" />
      <div className="runtime-failure-heading">
        <AlertTriangle aria-hidden="true" size={16} />
        <h1>Local runtime unavailable</h1>
      </div>
      <p role="alert">{error}</p>
      <button className="button button-primary" type="button" onClick={onRetry}>
        <RefreshCw aria-hidden="true" size={14} /> Retry
      </button>
    </main>
  );
}

export function MetricStrip({
  metrics,
}: {
  metrics: { value: string; label: string; tone?: "success" | "warning" | "danger" }[];
}) {
  return (
    <div className="metric-strip">
      {metrics.map((metric) => (
        <div className="metric-cell" key={metric.label}>
          <strong>{metric.value}</strong>
          <StatusText tone={metric.tone ?? "muted"}>{metric.label}</StatusText>
        </div>
      ))}
    </div>
  );
}

function formatAudioTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${Math.floor(seconds % 60).toString().padStart(2, "0")}`;
}

export function CompactAudioPlayer({ src, label }: { src?: string; label: string }) {
  const audio = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState(false);

  useEffect(() => {
    audio.current?.pause();
    setPlaying(false);
    setTime(0);
    setDuration(0);
    setError(false);
  }, [src]);

  async function toggle() {
    if (!audio.current || !src) return;
    if (!audio.current.paused) {
      audio.current.pause();
      return;
    }
    try {
      await audio.current.play();
      setError(false);
    } catch {
      setError(true);
    }
  }

  function seek(event: MouseEvent<HTMLButtonElement>) {
    if (!audio.current || !duration) return;
    const box = event.currentTarget.getBoundingClientRect();
    audio.current.currentTime = Math.max(0, Math.min(duration, ((event.clientX - box.left) / box.width) * duration));
  }

  const progress = duration > 0 ? Math.min(1, time / duration) : 0;

  return (
    <div className={`compact-audio-player ${error ? "has-error" : ""}`}>
      {src ? <audio ref={audio} className="visually-hidden" preload="metadata" src={src} onCanPlay={() => setError(false)} onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)} onTimeUpdate={(event) => setTime(event.currentTarget.currentTime)} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} onError={() => setError(true)} /> : null}
      <button className="icon-button" type="button" title={playing ? `Pause ${label}` : `Play ${label}`} disabled={!src || error} onClick={() => void toggle()}>
        {playing ? <Pause aria-hidden="true" fill="currentColor" size={12} /> : <Play aria-hidden="true" fill="currentColor" size={12} />}
      </button>
      <button className="compact-audio-track" type="button" aria-label={`Seek ${label}`} disabled={!src || !duration} onClick={seek}>
        <i style={{ width: `${progress * 100}%` }} />
      </button>
      <span>{error ? "Unavailable" : `${formatAudioTime(time)} / ${formatAudioTime(duration)}`}</span>
    </div>
  );
}

export function EmptyState({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="empty-state">
      <strong>{title}</strong>
      <span>{detail}</span>
    </div>
  );
}
