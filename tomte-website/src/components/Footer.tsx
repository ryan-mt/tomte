import Link from "next/link";
import { GithubLogo } from "@phosphor-icons/react/dist/ssr";
import { nav, site } from "@/lib/content";

export function Footer() {
  return (
    <footer className="border-t border-line-2 bg-bg-2">
      <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8">
        <div className="grid gap-10 md:grid-cols-[1.7fr_1fr_1fr]">
          <div>
            <div className="flex items-baseline gap-2">
              <span className="font-display text-[26px] font-semibold tracking-tight text-ink">
                tomte
              </span>
              <span className="inline-block size-[7px] rounded-full bg-hearth" aria-hidden="true" />
            </div>
            <p className="mt-4 max-w-xs text-[15px] leading-snug text-ink-2">
              The coding agent that proves its work. One binary, any model,
              and a receipt for everything.
            </p>
            <p className="mt-4 font-mono text-[11px] uppercase tracking-[0.16em] text-ink-3">
              Rust. Provider-agnostic. MIT.
            </p>
          </div>

          <nav className="flex flex-col gap-2.5">
            <span className="mono-label mb-1">Navigate</span>
            {nav.map((item) => (
              <Link
                key={item.href}
                href={item.href}
                className="text-[14.5px] text-ink-2 transition-colors hover:text-hearth"
              >
                {item.label}
              </Link>
            ))}
          </nav>

          <div className="flex flex-col gap-2.5">
            <span className="mono-label mb-1">Source</span>
            <a href={site.repoUrl} target="_blank" rel="noreferrer" className="flex items-center gap-2 text-[14.5px] text-ink-2 transition-colors hover:text-hearth">
              <GithubLogo size={15} weight="regular" />
              Repository
            </a>
            <a href={site.latestReleaseUrl} target="_blank" rel="noreferrer" className="text-[14.5px] text-ink-2 transition-colors hover:text-hearth">
              Latest release
            </a>
            <a href={site.contributingUrl} target="_blank" rel="noreferrer" className="text-[14.5px] text-ink-2 transition-colors hover:text-hearth">
              Contributing
            </a>
            <a href={site.licenseUrl} target="_blank" rel="noreferrer" className="text-[14.5px] text-ink-2 transition-colors hover:text-hearth">
              License (MIT)
            </a>
          </div>
        </div>

        <div className="mt-14 flex flex-col gap-2 border-t border-line pt-6 sm:flex-row sm:items-center sm:justify-between">
          <p className="font-mono text-[11px] leading-relaxed text-ink-3">
            Set in Fraunces, Spline Sans, and IBM Plex Mono. Documentation
            tracks the latest release.
          </p>
          <p className="font-mono text-[11px] text-ink-3">
            Done means verified.
          </p>
        </div>
      </div>
    </footer>
  );
}
