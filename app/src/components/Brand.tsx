export function BrandMark({ className = "", label = "soundAr" }: { className?: string; label?: string }) {
  return (
    <svg className={`brand-mark ${className}`} viewBox="0 0 512 512" role="img" aria-label={label}>
      <g className="brand-mark-primary" fill="none" strokeLinecap="round" strokeLinejoin="round">
        <path d="M382 144C345 93 278 70 214 80C123 95 63 181 82 270C101 358 193 413 279 383C324 367 359 335 379 295" strokeWidth="72" />
        <path d="M379 255V407" strokeWidth="72" />
        <path d="M110 255H142L159 226L180 285L204 202L230 305L257 222L280 286L302 240L322 268H385" strokeWidth="28" />
      </g>
      <g className="brand-mark-accent" fill="none" strokeLinecap="round">
        <path d="M448 233Q466 255 448 277" strokeWidth="14" />
        <path d="M469 216Q498 255 469 294" strokeWidth="14" />
      </g>
      <circle className="brand-mark-accent-fill" cx="420" cy="255" r="12" />
    </svg>
  );
}

export function BrandLockup({ className = "", tagline }: { className?: string; tagline?: string }) {
  return (
    <div className={`brand-lockup ${className}`} aria-label={tagline ? `soundAr, ${tagline}` : "soundAr"}>
      <BrandMark />
      <div className="brand-lockup-copy">
        <strong>soundAr</strong>
        {tagline ? <span>{tagline}</span> : null}
      </div>
    </div>
  );
}
