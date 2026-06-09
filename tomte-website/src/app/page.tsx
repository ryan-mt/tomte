import Link from "next/link";
import { ArrowRight, GithubLogo } from "@phosphor-icons/react/dist/ssr";
import {
  site,
  proofs,
  vitals,
  manners,
  capabilities,
  providers,
  toolCount,
} from "@/lib/content";
import { Reveal } from "@/components/Reveal";

/* ---------------------------------------------------------------------------
   Home: the night watch. Every section is dressed as evidence — capsules,
   receipts, stamps — because that is the product: claims you can check.
--------------------------------------------------------------------------- */

function Hearthfire() {
  return (
    <span className="hearthfire" aria-hidden="true">
      <span /><span /><span /><span /><span /><span />
    </span>
  );
}

/** A terminal excerpt dressed as the receipt it is. */
function Excerpt({ command, lines }: { command: string; lines: string[] }) {
  return (
    <div className="bg-code overflow-hidden rounded-lg border border-line-2">
      <div className="flex items-center gap-2 border-b border-line px-3.5 py-2">
        <span className="size-[7px] rounded-full bg-line-2" aria-hidden="true" />
        <span className="size-[7px] rounded-full bg-line-2" aria-hidden="true" />
        <span className="font-mono text-[11px] text-ink-3">tomte</span>
      </div>
      <pre className="overflow-x-auto px-3.5 py-3 font-mono text-[11.5px] leading-[1.7] text-codefg">
        <span className="text-hearth">$ </span>
        <span className="text-ink">{command}</span>
        {"\n"}
        {lines.join("\n")}
      </pre>
    </div>
  );
}

export default function Home() {
  return (
    <>
      {/* ── Hero: the farm at night ─────────────────────────────────────── */}
      <section className="grain relative overflow-hidden border-b border-line">
        <div className="aurora" aria-hidden="true" />
        <div className="relative mx-auto grid max-w-[1200px] gap-12 px-5 pb-20 pt-16 sm:px-8 lg:grid-cols-[1.05fr_0.95fr] lg:items-center lg:pb-28 lg:pt-24">
          <div>
            <Reveal>
              <p className="mono-label flex items-center gap-3">
                <Hearthfire />
                the night watch · v{site.version} · MIT · Rust
              </p>
            </Reveal>
            <Reveal delay={0.08}>
              <h1 className="mt-6 text-[44px] sm:text-[60px] lg:text-[68px]">
                Done means{" "}
                <em className="font-medium italic text-hearth">verified.</em>
              </h1>
            </Reveal>
            <Reveal delay={0.16}>
              <p className="mt-6 max-w-[34rem] text-[17px] leading-relaxed text-ink-2">
                {site.subhead}
              </p>
            </Reveal>
            <Reveal delay={0.24}>
              <div className="mt-9 flex flex-wrap items-center gap-4">
                <Link
                  href="/install"
                  className="group inline-flex items-center gap-2 rounded-md bg-hearth px-5 py-3 font-mono text-[13px] font-medium uppercase tracking-[0.1em] text-bg transition-colors hover:bg-ink"
                >
                  Install
                  <ArrowRight
                    size={15}
                    weight="bold"
                    className="transition-transform group-hover:translate-x-0.5"
                  />
                </Link>
                <a
                  href={site.repoUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-2 rounded-md border border-line-2 px-5 py-3 font-mono text-[13px] uppercase tracking-[0.1em] text-ink-2 transition-colors hover:border-ink-3 hover:text-ink"
                >
                  <GithubLogo size={16} weight="regular" />
                  Source
                </a>
              </div>
            </Reveal>
            <Reveal delay={0.3}>
              <p className="mt-10 max-w-[32rem] font-display text-[15px] italic leading-relaxed text-ink-3">
                Named for the Nordic farm spirit who keeps the household in
                order overnight: meticulous, quiet, and intolerant of sloppy
                work.
              </p>
            </Reveal>
          </div>

          {/* The proof capsule, as it lands in the terminal. */}
          <Reveal delay={0.2} y={20}>
            <div className="capsule p-4 sm:p-5">
              <div className="flex items-center justify-between pb-3">
                <span className="mono-label">proof capsule</span>
                <span className="stamp">verified</span>
              </div>
              <Excerpt command={proofs[0].command} lines={proofs[0].excerpt} />
              <div className="tear my-4" aria-hidden="true" />
              <p className="text-[13px] leading-relaxed text-ink-3">
                {proofs[0].honest}
              </p>
            </div>
          </Reveal>
        </div>
      </section>

      {/* ── The four proofs ─────────────────────────────────────────────── */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 lg:py-28">
          <Reveal>
            <p className="mono-label">what no other terminal agent ships together</p>
            <h2 className="mt-4 max-w-[26ch] text-[34px] sm:text-[44px]">
              Most agents tell you the work is done. This one shows receipts.
            </h2>
          </Reveal>

          <div className="mt-14 grid gap-6 lg:grid-cols-2">
            {proofs.map((p, i) => (
              <Reveal key={p.key} delay={0.06 * i}>
                <article className="capsule flex h-full flex-col p-5 sm:p-6">
                  <div className="flex items-start justify-between gap-4">
                    <h3 className="text-[22px] sm:text-[24px]">{p.title}</h3>
                    <span className="stamp shrink-0">{p.stamp}</span>
                  </div>
                  <p className="mt-3 text-[14.5px] leading-relaxed text-ink-2">
                    {p.body}
                  </p>
                  <div className="mt-5">
                    <Excerpt command={p.command} lines={p.excerpt} />
                  </div>
                  <div className="tear my-4" aria-hidden="true" />
                  <p className="mt-auto text-[12.5px] leading-relaxed text-ink-3">
                    {p.honest}
                  </p>
                </article>
              </Reveal>
            ))}
          </div>
        </div>
      </section>

      {/* ── Composed vitals: pulse + handoff ────────────────────────────── */}
      <section className="grain relative border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 lg:py-24">
          <Reveal>
            <p className="mono-label">because the indexes are real data, they compose</p>
            <h2 className="mt-4 text-[30px] sm:text-[38px]">
              The map answers questions you haven&apos;t asked yet.
            </h2>
          </Reveal>
          <div className="mt-12 grid gap-6 md:grid-cols-2">
            {vitals.map((v, i) => (
              <Reveal key={v.key} delay={0.08 * i}>
                <article className="capsule h-full bg-bg p-5 sm:p-6">
                  <h3 className="text-[21px]">{v.title}</h3>
                  <p className="mt-3 text-[14px] leading-relaxed text-ink-2">
                    {v.body}
                  </p>
                  <div className="mt-5">
                    <Excerpt command={v.command} lines={v.excerpt} />
                  </div>
                </article>
              </Reveal>
            ))}
          </div>
        </div>
      </section>

      {/* ── The keeper's manner ─────────────────────────────────────────── */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 lg:py-24">
          <Reveal>
            <p className="mono-label">the keeper&apos;s manner</p>
            <h2 className="mt-4 text-[30px] sm:text-[38px]">
              Quiet habits, wrapped around the proofs.
            </h2>
          </Reveal>
          <div className="mt-12 grid gap-px overflow-hidden rounded-xl border border-line bg-line sm:grid-cols-2">
            {manners.map((m, i) => (
              <Reveal key={m.title} delay={0.05 * i} className="h-full">
                <div className="h-full bg-bg p-6">
                  <h3 className="text-[18px]">{m.title}</h3>
                  <p className="mt-2.5 text-[13.5px] leading-relaxed text-ink-2">
                    {m.body}
                  </p>
                </div>
              </Reveal>
            ))}
          </div>
        </div>
      </section>

      {/* ── Multi-model ─────────────────────────────────────────────────── */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 lg:py-24">
          <Reveal>
            <p className="mono-label">one binary, any brain</p>
            <h2 className="mt-4 max-w-[24ch] text-[30px] sm:text-[38px]">
              The trail, the map, and the proofs survive a model switch.
            </h2>
            <p className="mt-4 max-w-[40rem] text-[14.5px] leading-relaxed text-ink-2">
              Sign in with a subscription or an API key. Switch mid-session
              with /model. Everything tomte records is provider-agnostic, so
              the why written by one model is read by the next.
            </p>
          </Reveal>
          <div className="mt-10 grid gap-6 md:grid-cols-3">
            {providers.map((p, i) => (
              <Reveal key={p.key} delay={0.06 * i} className="h-full">
                <article className="capsule h-full bg-bg p-5">
                  <div className="flex items-center justify-between">
                    <h3 className="text-[18px]">{p.name}</h3>
                    <span
                      className={`size-2 rounded-full ${
                        p.accent === "oai"
                          ? "bg-oai"
                          : p.accent === "ant"
                            ? "bg-ant"
                            : "bg-compat"
                      }`}
                      aria-hidden="true"
                    />
                  </div>
                  <p className="mono-label mt-2">{p.tag}</p>
                  <p className="mt-3 text-[13px] leading-relaxed text-ink-2">
                    {p.body}
                  </p>
                </article>
              </Reveal>
            ))}
          </div>
          <Reveal delay={0.2}>
            <Link
              href="/models"
              className="group mt-8 inline-flex items-center gap-2 font-mono text-[12px] uppercase tracking-[0.14em] text-hearth transition-colors hover:text-ink"
            >
              the full catalogue
              <ArrowRight size={13} weight="bold" className="transition-transform group-hover:translate-x-0.5" />
            </Link>
          </Reveal>
        </div>
      </section>

      {/* ── Table stakes, done well ─────────────────────────────────────── */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-20 sm:px-8 lg:py-24">
          <Reveal>
            <p className="mono-label">and the table stakes, done well</p>
            <h2 className="mt-4 text-[30px] sm:text-[38px]">
              {toolCount} tools, zero daemons.
            </h2>
          </Reveal>
          <div className="mt-12 grid gap-x-10 gap-y-8 sm:grid-cols-2 lg:grid-cols-4">
            {capabilities.map((c, i) => (
              <Reveal key={c.tag} delay={0.04 * i}>
                <div>
                  <p className="mono-label text-hearth">{c.tag}</p>
                  <h3 className="mt-2 text-[16.5px]">{c.title}</h3>
                  <p className="mt-2 text-[13px] leading-relaxed text-ink-3">
                    {c.body}
                  </p>
                </div>
              </Reveal>
            ))}
          </div>
          <Reveal delay={0.2}>
            <Link
              href="/field-guide"
              className="group mt-10 inline-flex items-center gap-2 font-mono text-[12px] uppercase tracking-[0.14em] text-hearth transition-colors hover:text-ink"
            >
              the full field guide
              <ArrowRight size={13} weight="bold" className="transition-transform group-hover:translate-x-0.5" />
            </Link>
          </Reveal>
        </div>
      </section>

      {/* ── Closing CTA ─────────────────────────────────────────────────── */}
      <section className="grain relative overflow-hidden">
        <div className="aurora" aria-hidden="true" />
        <div className="relative mx-auto max-w-[1200px] px-5 py-24 text-center sm:px-8 lg:py-32">
          <Reveal>
            <p className="flex items-center justify-center">
              <Hearthfire />
            </p>
            <h2 className="mx-auto mt-6 max-w-[20ch] text-[36px] sm:text-[48px]">
              Let the keeper take the night shift.
            </h2>
            <div className="mt-9 flex flex-wrap items-center justify-center gap-4">
              <Link
                href="/install"
                className="inline-flex items-center gap-2 rounded-md bg-hearth px-6 py-3.5 font-mono text-[13px] font-medium uppercase tracking-[0.1em] text-bg transition-colors hover:bg-ink"
              >
                Install tomte
                <ArrowRight size={15} weight="bold" />
              </Link>
              <code className="rounded-md border border-line-2 bg-code px-4 py-3 font-mono text-[13px] text-codefg">
                tomte prove
              </code>
            </div>
          </Reveal>
        </div>
      </section>
    </>
  );
}
