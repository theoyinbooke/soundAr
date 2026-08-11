import { Check, ChevronDown, LoaderCircle } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";

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
  return (
    <header className="page-header">
      <div className="page-heading">
        <h1>{title}</h1>
        <p>{subtitle}</p>
      </div>
      {actions ? <div className="page-actions">{actions}</div> : null}
    </header>
  );
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
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  status?: string;
}) {
  return (
    <div className="field field-select">
      <span className="field-label">{label}</span>
      <Dropdown ariaLabel={label} value={value} onChange={onChange} options={options} status={status} />
    </div>
  );
}

export function Dropdown({
  ariaLabel,
  value,
  onChange,
  options,
  status,
}: {
  ariaLabel: string;
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  status?: string;
}) {
  const [open, setOpen] = useState(false);
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

  return (
    <div className={`dropdown ${open ? "is-open" : ""}`} ref={root}>
      <button
        className="dropdown-trigger"
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-expanded={open}
        aria-controls={`${ariaLabel.replace(/\s+/g, "-").toLowerCase()}-options`}
        onClick={() => setOpen((current) => !current)}
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
      <LoaderCircle className="spin" aria-hidden="true" size={20} />
      <span>Reading the local runtime…</span>
    </div>
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

export function EmptyState({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="empty-state">
      <strong>{title}</strong>
      <span>{detail}</span>
    </div>
  );
}
