import Link from "next/link";
import { GithubLogo, List } from "@phosphor-icons/react/dist/ssr";
import { site } from "@/lib/content";

const links = [
  { label: "Field guide", href: "/field-guide" },
  { label: "Models", href: "/models" },
];

export function Nav() {
  return (
    <header className="sticky top-0 z-50 border-b border-line bg-bg/85 backdrop-blur-md">
      <div className="mx-auto flex h-[68px] max-w-[1200px] items-center justify-between px-5 sm:px-8">
        <Link href="/" className="flex items-center gap-1.5" aria-label="tomte home">
          <span className="font-display text-[20px] font-extrabold tracking-tight text-ink">
            tomte
          </span>
          <span className="mb-[3px] inline-block h-[15px] w-[8px] bg-ink" aria-hidden="true" />
        </Link>

        <nav className="hidden items-center gap-8 md:flex">
          {links.map((item) => (
            <Link
              key={item.href}
              href={item.href}
              className="font-mono text-[12px] uppercase tracking-[0.14em] text-ink-2 transition-colors hover:text-ink"
            >
              {item.label}
            </Link>
          ))}
          <a
            href={site.repoUrl}
            target="_blank"
            rel="noreferrer"
            aria-label="Tomte on GitHub"
            className="text-ink-2 transition-colors hover:text-ink"
          >
            <GithubLogo size={19} weight="regular" />
          </a>
          <Link
            href="/install"
            className="bg-ink px-4 py-2 font-mono text-[12px] uppercase tracking-[0.14em] text-bg transition-colors hover:bg-ink-2"
          >
            Install
          </Link>
        </nav>

        <details className="group relative md:hidden">
          <summary className="flex cursor-pointer items-center text-ink [&::-webkit-details-marker]:hidden">
            <List size={22} weight="regular" />
          </summary>
          <div className="absolute right-0 top-[calc(100%+0.9rem)] z-50 w-52 border border-ink bg-bg p-1.5">
            {links.map((item) => (
              <Link
                key={item.href}
                href={item.href}
                className="block px-3 py-2 font-mono text-[12px] uppercase tracking-[0.14em] text-ink-2 hover:bg-bg-2 hover:text-ink"
              >
                {item.label}
              </Link>
            ))}
            <a
              href={site.repoUrl}
              target="_blank"
              rel="noreferrer"
              className="flex items-center gap-2 px-3 py-2 font-mono text-[12px] uppercase tracking-[0.14em] text-ink-2 hover:bg-bg-2 hover:text-ink"
            >
              <GithubLogo size={15} weight="regular" />
              GitHub
            </a>
            <Link
              href="/install"
              className="mt-1.5 block bg-ink px-3 py-2 text-center font-mono text-[12px] uppercase tracking-[0.14em] text-bg"
            >
              Install
            </Link>
          </div>
        </details>
      </div>
    </header>
  );
}
