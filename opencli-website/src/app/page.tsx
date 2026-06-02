import Link from "next/link";
import { ArrowRight, GithubLogo } from "@phosphor-icons/react/dist/ssr";
import { site, capabilities, toolBelt, toolCount } from "@/lib/content";
import { Switchboard } from "@/components/Switchboard";
import { CodeBlock } from "@/components/CodeBlock";
import { Reveal } from "@/components/Reveal";

const allTools = toolBelt.flatMap((g) => g.tools);

const sessionDemo = `opencli                       # launch the terminal UI
opencli chat "add a test for the parser"
opencli chat --model claude-opus-4-8 --reasoning high "refactor auth"
echo "summarize CLAUDE.md" | opencli chat`;

export default function Home() {
  return (
    <>
      {/* Hero: type-forward Swiss. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 pb-16 pt-16 sm:px-8 sm:pb-24 sm:pt-24">
          <p className="mono-label">Open-source coding agent for your terminal</p>
          <h1 className="mt-5 max-w-[22ch] font-display text-[2.9rem] font-extrabold leading-[0.92] tracking-[-0.035em] text-ink sm:text-[4.25rem]">
            A coding agent that lives in your terminal.
          </h1>
          <p className="mt-6 max-w-xl text-[1.1875rem] leading-relaxed text-ink-2">
            {site.subhead}
          </p>
          <div className="mt-9 flex flex-col gap-3 sm:flex-row">
            <Link
              href="/install"
              className="inline-flex items-center justify-center gap-2 bg-ink px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-bg transition-colors hover:bg-ink-2"
            >
              Install
              <ArrowRight size={15} weight="bold" />
            </Link>
            <a
              href={site.repoUrl}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center justify-center gap-2 border border-ink px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-ink transition-colors hover:bg-ink hover:text-bg"
            >
              <GithubLogo size={15} weight="regular" />
              View source
            </a>
          </div>
        </div>
      </section>

      {/* Signature: the provider switchboard. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <Switchboard />
        </div>
      </section>

      {/* Capabilities: divided index. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="max-w-2xl font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
            Why you might reach for it
          </h2>
          <div className="mt-12 grid gap-x-14 gap-y-10 sm:grid-cols-2">
            {capabilities.map((cap, i) => (
              <Reveal key={cap.tag} delay={(i % 2) * 0.05}>
                <div className="border-t-2 border-ink pt-4">
                  <div className="flex items-baseline justify-between gap-4">
                    <h3 className="font-display text-[1.4rem] font-bold leading-snug text-ink">
                      {cap.title}
                    </h3>
                    <span className="shrink-0 font-mono text-[11px] text-ink-3">{cap.tag}</span>
                  </div>
                  <p className="mt-2.5 text-[15px] leading-relaxed text-ink-2">{cap.body}</p>
                </div>
              </Reveal>
            ))}
          </div>
        </div>
      </section>

      {/* Tool belt: mono tag field. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <div className="flex flex-col gap-5 sm:flex-row sm:items-end sm:justify-between">
            <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
              A tool belt of {toolCount}
            </h2>
            <Link
              href="/field-guide"
              className="inline-flex shrink-0 items-center gap-2 font-mono text-[12.5px] uppercase tracking-[0.12em] text-ink transition-colors hover:text-ink-2"
            >
              Open the field guide
              <ArrowRight size={14} weight="bold" />
            </Link>
          </div>
          <p className="mt-4 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
            Files, shell, search, web, notebooks, sub-agents, and flow control. Streamed, schema-validated, and run in parallel where it is safe.
          </p>
          <div className="mt-10 flex flex-wrap gap-2">
            {allTools.map((tool) => (
              <span
                key={tool}
                className="border border-line-2 bg-bg px-2.5 py-1 font-mono text-[12.5px] text-ink-2"
              >
                {tool}
              </span>
            ))}
          </div>
        </div>
      </section>

      {/* Two ways to run it: split text + terminal slab. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto grid max-w-[1200px] items-center gap-10 px-5 py-16 sm:px-8 sm:py-24 lg:grid-cols-[0.9fr_1.1fr] lg:gap-14">
          <div>
            <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
              Two ways to run it
            </h2>
            <p className="mt-4 max-w-md text-[1.0625rem] leading-relaxed text-ink-2">
              Launch the full terminal UI, or go headless for scripts, cron, and systemd. Same agent either way.
            </p>
            <p className="mt-4 font-mono text-[12.5px] text-ink-3">
              opencli, opencli resume, opencli chat, opencli run
            </p>
          </div>
          <CodeBlock label="terminal" code={sessionDemo} />
        </div>
      </section>

      {/* Closing CTA: the single inverted block. */}
      <section className="bg-ink text-bg">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 sm:py-28">
          <h2 className="max-w-2xl font-display text-[2.6rem] font-extrabold leading-[0.92] tracking-[-0.03em] text-bg sm:text-[4rem]">
            Install in about a minute.
          </h2>
          <p className="mt-5 max-w-md text-[1.0625rem] leading-relaxed text-bg/65">
            One binary on your PATH, then sign in. Build from source or grab a prebuilt archive for your platform.
          </p>
          <div className="mt-9 flex flex-col gap-3 sm:flex-row">
            <Link
              href="/install"
              className="inline-flex items-center justify-center gap-2 bg-bg px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-ink transition-colors hover:bg-[#e7e7e0]"
            >
              Install
              <ArrowRight size={15} weight="bold" />
            </Link>
            <a
              href={site.repoUrl}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center justify-center gap-2 border border-bg/35 px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-bg transition-colors hover:border-bg"
            >
              <GithubLogo size={15} weight="regular" />
              View source
            </a>
          </div>
        </div>
      </section>
    </>
  );
}
