import type { Metadata } from "next";
import Link from "next/link";
import {
  ArrowRight,
  FileText,
  MagnifyingGlass,
  Terminal,
  Globe,
  ListChecks,
  UsersThree,
  GitBranch,
  Notebook,
  Plus,
  Stamp,
} from "@phosphor-icons/react/dist/ssr";
import type { ComponentType } from "react";
import {
  site,
  proofs,
  vitals,
  toolBelt,
  toolCount,
  reasoningLevels,
  slashCommands,
  composerPrefixes,
  security,
  faq,
} from "@/lib/content";
import { PageHeader } from "@/components/PageHeader";
import { Reveal } from "@/components/Reveal";

export const metadata: Metadata = {
  title: "Field guide",
  description:
    "The full tomte field guide: the evidence commands, the tool belt, reasoning levels, slash commands, and the security model.",
};

type IconCmp = ComponentType<{
  size?: number;
  weight?: "thin" | "light" | "regular" | "bold" | "fill" | "duotone";
  className?: string;
}>;

const GROUP_ICON: Record<string, IconCmp> = {
  Files: FileText,
  Search: MagnifyingGlass,
  Shell: Terminal,
  Web: Globe,
  Flow: ListChecks,
  Agents: UsersThree,
  "Git worktrees": GitBranch,
  Notebooks: Notebook,
};

export default function FieldGuide() {
  return (
    <>
      <PageHeader
        kicker="The field guide"
        title="The keeper's duties, in full."
        intro={`The evidence commands no other terminal agent ships, the ${toolCount}-tool belt, how it reasons, the slash commands worth knowing, and the security model, stated plainly.`}
      />

      {/* The evidence commands. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <Reveal>
            <p className="mono-label flex items-center gap-2">
              <Stamp size={14} weight="duotone" className="text-hearth" />
              the evidence commands
            </p>
            <h2 className="mt-4 text-[2rem] sm:text-[2.6rem]">
              No model in the loop. Safe anywhere, scriptable everywhere.
            </h2>
          </Reveal>
          <dl className="mt-10 overflow-hidden rounded-xl border border-line-2">
            {[
              ...proofs.map((p) => ({ cmd: p.command, desc: p.title, json: true })),
              ...vitals.map((v) => ({ cmd: v.command, desc: v.title, json: true })),
            ].map((row) => (
              <div
                key={row.cmd}
                className="grid gap-2 border-b border-line bg-bg-2 px-5 py-3.5 last:border-b-0 sm:grid-cols-[minmax(0,26rem)_1fr] sm:gap-6"
              >
                <dt className="font-mono text-[13px] text-hearth">{row.cmd}</dt>
                <dd className="text-[13.5px] leading-snug text-ink-2">
                  {row.desc}
                </dd>
              </div>
            ))}
          </dl>
          <p className="mt-5 max-w-2xl font-mono text-[12px] leading-relaxed text-ink-3">
            Each takes --json for scripts. tomte prove exits non-zero on a
            failing check, so it gates a commit hook or CI step.
          </p>
        </div>
      </section>

      {/* Tool belt. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <Reveal>
            <p className="mono-label">the tool belt</p>
            <h2 className="mt-4 text-[2rem] sm:text-[2.6rem]">
              {toolCount} tools, streamed and schema-validated.
            </h2>
          </Reveal>
          <div className="mt-10 grid gap-5 sm:grid-cols-2">
            {toolBelt.map((g, i) => {
              const GroupIcon = GROUP_ICON[g.group];
              return (
                <Reveal key={g.group} delay={(i % 2) * 0.05}>
                  <div className="capsule flex h-full flex-col bg-bg p-5">
                    <div className="flex items-center gap-3">
                      {GroupIcon ? (
                        <GroupIcon size={22} weight="duotone" className="text-hearth" />
                      ) : null}
                      <h3 className="text-[1.3rem] leading-none">{g.group}</h3>
                    </div>
                    <p className="mt-3 text-[14px] leading-relaxed text-ink-2">{g.blurb}</p>
                    <div className="mt-4 flex flex-wrap gap-1.5">
                      {g.tools.map((t) => (
                        <span
                          key={t}
                          className="rounded border border-line-2 bg-bg-3 px-2 py-0.5 font-mono text-[12px] text-ink-2"
                        >
                          {t}
                        </span>
                      ))}
                    </div>
                  </div>
                </Reveal>
              );
            })}
          </div>
          <p className="mt-7 max-w-2xl text-[14px] leading-relaxed text-ink-2">
            Stale-file guards refuse a write when a file changed since the
            model last read it. Destructive shell commands are flagged for
            confirmation, and incomplete streamed tool calls are dropped rather
            than executed with half-finished arguments.
          </p>
        </div>
      </section>

      {/* Reasoning. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="max-w-2xl text-[2rem] sm:text-[2.6rem]">
            Reasoning effort, dialled in.
          </h2>
          <p className="mt-4 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
            Choose how hard the model thinks, for the session or a single turn.
            The newest Claude models use adaptive thinking; OpenAI maps the
            same levels to its reasoning effort.
          </p>
          <div className="mt-8 flex flex-wrap items-center gap-2.5">
            {reasoningLevels.map((level, i) => (
              <span key={level} className="flex items-center gap-2.5">
                <span className="rounded-md border border-line-2 bg-bg-2 px-4 py-2 font-mono text-[13px] text-ink">
                  {level}
                </span>
                {i < reasoningLevels.length - 1 ? (
                  <ArrowRight size={13} weight="bold" className="text-ink-3" />
                ) : null}
              </span>
            ))}
          </div>
          <p className="mt-5 font-mono text-[12.5px] text-ink-3">
            Set it with tomte config --set-reasoning high, or /thinking inside
            the session.
          </p>
        </div>
      </section>

      {/* Slash commands + composer prefixes. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="text-[2rem] sm:text-[2.6rem]">
            Slash commands worth knowing.
          </h2>
          <div className="mt-10 grid gap-x-14 gap-y-10 sm:grid-cols-2">
            {slashCommands.map((group) => (
              <div key={group.group}>
                <h3 className="text-[1.25rem] text-hearth">{group.group}</h3>
                <dl className="mt-3 border-t border-line-2">
                  {group.items.map((item) => (
                    <div
                      key={item.cmd}
                      className="grid grid-cols-[10rem_1fr] gap-4 border-b border-line py-2.5"
                    >
                      <dt className="font-mono text-[13px] font-medium text-ink">{item.cmd}</dt>
                      <dd className="text-[13.5px] leading-snug text-ink-2">{item.desc}</dd>
                    </div>
                  ))}
                </dl>
              </div>
            ))}
          </div>

          <div className="mt-12 border-t border-line-2 pt-8">
            <h3 className="text-[1.25rem]">Composer prefixes</h3>
            <p className="mt-2 max-w-2xl text-[14px] leading-relaxed text-ink-2">
              Three characters you type at the start of a line: quick inline
              actions without leaving the composer.
            </p>
            <dl className="mt-5 border-t border-line">
              {composerPrefixes.map((p) => (
                <div key={p.prefix} className="grid grid-cols-[5.5rem_1fr] gap-5 border-b border-line py-3">
                  <dt className="font-mono text-[14px] font-medium text-hearth">{p.prefix}</dt>
                  <dd className="text-[14px] leading-snug text-ink-2">{p.desc}</dd>
                </div>
              ))}
            </dl>
          </div>
        </div>
      </section>

      {/* Security. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="max-w-2xl text-[2rem] sm:text-[2.6rem]">
            The security model, stated plainly.
          </h2>
          <p className="mt-4 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
            run_shell runs inside an OS-level sandbox, confined to the
            workspace with the network off by default. On Windows that
            confinement is best-effort, so review destructive prompts there.
            Here is the rest of what tomte guards.
          </p>
          <div className="mt-9 grid gap-x-14 gap-y-8 sm:grid-cols-2">
            {security.map((s) => (
              <div key={s.title} className="border-t border-line-2 pt-4">
                <h3 className="text-[1.25rem] leading-snug">{s.title}</h3>
                <p className="mt-2 text-[14px] leading-relaxed text-ink-2">{s.body}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* FAQ. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[820px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="text-[2rem] sm:text-[2.6rem]">Questions.</h2>
          <div className="mt-8 border-t border-line-2">
            {faq.map((item) => (
              <details key={item.q} className="group border-b border-line">
                <summary className="flex cursor-pointer list-none items-center justify-between gap-5 py-4 font-display text-[1.2rem] font-medium leading-snug text-ink [&::-webkit-details-marker]:hidden">
                  {item.q}
                  <Plus size={18} weight="bold" className="shrink-0 text-hearth transition-transform duration-200 group-open:rotate-45" />
                </summary>
                <p className="max-w-[68ch] pb-5 pr-6 text-[14.5px] leading-relaxed text-ink-2">{item.a}</p>
              </details>
            ))}
          </div>
        </div>
      </section>

      {/* CTA. */}
      <section>
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-20">
          <div className="capsule grain relative overflow-hidden px-6 py-12 text-center sm:px-10 sm:py-16">
            <h2 className="text-[2rem] sm:text-[2.5rem]">
              Put the keeper in your terminal.
            </h2>
            <p className="mx-auto mt-4 max-w-md text-[1.0625rem] leading-relaxed text-ink-2">
              One binary, then sign in. The full agent in under a minute.
            </p>
            <div className="mt-8 flex flex-col justify-center gap-3 sm:flex-row">
              <Link
                href="/install"
                className="inline-flex items-center justify-center gap-2 rounded-md bg-hearth px-7 py-3.5 font-mono text-[12.5px] font-medium uppercase tracking-[0.14em] text-bg transition-colors hover:bg-ink"
              >
                Install
                <ArrowRight size={15} weight="bold" />
              </Link>
              <a
                href={site.repoUrl}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center justify-center gap-2 rounded-md border border-line-2 px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-ink-2 transition-colors hover:border-ink-3 hover:text-ink"
              >
                View source
              </a>
            </div>
          </div>
        </div>
      </section>
    </>
  );
}
