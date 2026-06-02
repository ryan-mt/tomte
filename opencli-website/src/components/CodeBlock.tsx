/**
 * Inverted code block: a near-black terminal slab on the light Swiss canvas.
 * Shows real commands or config. Not a fake terminal with traffic lights.
 */
export function CodeBlock({
  label,
  code,
  className = "",
}: {
  label?: string;
  code: string;
  className?: string;
}) {
  return (
    <div className={`border border-ink bg-code ${className}`}>
      {label ? (
        <div className="flex items-center border-b border-white/10 px-3.5 py-2">
          <span className="font-mono text-[11px] uppercase tracking-[0.16em] text-white/45">
            {label}
          </span>
        </div>
      ) : null}
      <pre className="overflow-x-auto px-4 py-3.5 font-mono text-[12.5px] leading-[1.7] text-codefg">
        <code>{code}</code>
      </pre>
    </div>
  );
}
