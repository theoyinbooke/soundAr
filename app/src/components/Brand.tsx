export function BrandLockup({ className = "", tagline }: { className?: string; tagline?: string }) {
  return (
    <div className={`brand-lockup ${className}`} aria-label={tagline ? `soundAr, ${tagline}` : "soundAr"}>
      <div className="brand-lockup-copy">
        <strong>soundAr</strong>
        {tagline ? <span>{tagline}</span> : null}
      </div>
    </div>
  );
}
