/**
 * Code slab: the near-black terminal surface on the night canvas. Shows real
 * commands or config. Quiet chrome, no fake traffic lights.
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
    <div className={`overflow-hidden rounded-lg border border-line-2 bg-code ${className}`}>
      {label ? (
        <div className="flex items-center border-b border-line px-3.5 py-2">
          <span className="font-mono text-[11px] uppercase tracking-[0.16em] text-ink-3">
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
