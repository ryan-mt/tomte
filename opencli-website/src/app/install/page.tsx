import type { Metadata } from "next";
import Link from "next/link";
import { ArrowRight, DownloadSimple } from "@phosphor-icons/react/dist/ssr";
import {
  site,
  quickstart,
  binaries,
  authMethods,
  headlessExamples,
  configFields,
} from "@/lib/content";
import { PageHeader } from "@/components/PageHeader";
import { CodeBlock } from "@/components/CodeBlock";

export const metadata: Metadata = {
  title: "Install",
  description:
    "Install OpenCLI from source or a prebuilt binary, sign in with a subscription or an API key, and run it in the TUI or headless.",
};

const loginCommands = `opencli login                                 # OpenAI OAuth (ChatGPT subscription)
opencli login --api-key --provider openai     # paste an OpenAI key
opencli login --api-key --provider anthropic  # paste an Anthropic key
opencli status                                # who am I, and on what plan
opencli logout`;

const configJson = `{
  "model": "gpt-5.5",
  "reasoning_effort": "medium",
  "verbosity": "medium",
  "auto_approve_read": true,
  "auto_approve_write": false,
  "fallback_models": []
}`;

export default function Install() {
  return (
    <>
      <PageHeader
        kicker="Get started"
        title="Install and run."
        intro="A single binary, no daemon. Build from source or grab a prebuilt archive, sign in your way, then launch the terminal UI or drive it headless from a script."
      />

      {/* Quickstart. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
            Sixty-second start
          </h2>
          <div className="mt-10 border border-ink">
            {quickstart.map((q, i) => (
              <div
                key={q.step}
                className={`grid gap-5 p-5 sm:grid-cols-[1fr_1.3fr] sm:items-center sm:p-7 ${
                  i > 0 ? "border-t border-line-2" : ""
                }`}
              >
                <div className="flex gap-4">
                  <span className="font-mono text-[13px] text-ink-3">
                    {String(i + 1).padStart(2, "0")}
                  </span>
                  <div>
                    <h3 className="font-display text-[1.4rem] font-bold leading-none text-ink">
                      {q.step}
                    </h3>
                    <p className="mt-2 text-[14px] leading-relaxed text-ink-2">{q.note}</p>
                  </div>
                </div>
                <CodeBlock code={q.cmd} />
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Prebuilt binaries. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <div className="flex flex-col gap-5 sm:flex-row sm:items-end sm:justify-between">
            <div className="max-w-xl">
              <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
                Prefer a prebuilt binary
              </h2>
              <p className="mt-4 text-[1.0625rem] leading-relaxed text-ink-2">
                Grab the archive for your platform from the latest release and put opencli on your PATH.
              </p>
            </div>
            <a
              href={site.latestReleaseUrl}
              target="_blank"
              rel="noreferrer"
              className="inline-flex shrink-0 items-center gap-2 border border-ink px-5 py-2.5 font-mono text-[12.5px] uppercase tracking-[0.12em] text-ink transition-colors hover:bg-ink hover:text-bg"
            >
              <DownloadSimple size={15} weight="regular" />
              Latest release
            </a>
          </div>
          <div className="mt-9 border-t-2 border-ink">
            {binaries.map((b) => (
              <div
                key={b.platform}
                className="grid gap-2 border-b border-line py-4 sm:grid-cols-[1fr_2fr] sm:items-center"
              >
                <span className="font-display text-[1.2rem] font-bold text-ink">{b.platform}</span>
                <code className="font-mono text-[13px] text-ink-2">{b.archive}</code>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Sign in. */}
      <section className="border-b border-line">
        <div className="mx-auto grid max-w-[1200px] gap-12 px-5 py-16 sm:px-8 sm:py-24 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
          <div>
            <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
              Sign in your way
            </h2>
            <p className="mt-4 text-[1.0625rem] leading-relaxed text-ink-2">
              Four doors in: a subscription or an API key, OpenAI or Anthropic. OAuth uses PKCE and tokens refresh themselves.
            </p>
            <div className="mt-7 border-t-2 border-ink">
              {authMethods.map((a) => (
                <div key={a.title} className="border-b border-line py-4">
                  <h3 className="font-display text-[1.25rem] font-bold leading-snug text-ink">{a.title}</h3>
                  <p className="mt-1.5 text-[14px] leading-relaxed text-ink-2">{a.body}</p>
                </div>
              ))}
            </div>
          </div>
          <div className="lg:pt-2">
            <CodeBlock label="sign in" code={loginCommands} />
          </div>
        </div>
      </section>

      {/* Headless. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto grid max-w-[1200px] gap-12 px-5 py-16 sm:px-8 sm:py-24 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
          <div>
            <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
              Two ways to talk to it
            </h2>
            <p className="mt-4 text-[1.0625rem] leading-relaxed text-ink-2">
              Run opencli with no subcommand for the full terminal UI. Or go headless for scripts, cron, and systemd. Same agent either way.
            </p>
            <p className="mt-4 font-mono text-[12.5px] text-ink-3">
              opencli, opencli resume, opencli chat, opencli run
            </p>
          </div>
          <div className="lg:pt-2">
            <CodeBlock label="headless" code={headlessExamples.join("\n")} />
          </div>
        </div>
      </section>

      {/* Configuration. */}
      <section className="border-b border-line">
        <div className="mx-auto grid max-w-[1200px] gap-12 px-5 py-16 sm:px-8 sm:py-24 lg:grid-cols-[1.1fr_1fr] lg:gap-16">
          <div>
            <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
              Configuration
            </h2>
            <p className="mt-4 text-[1.0625rem] leading-relaxed text-ink-2">
              Settings live in config.json under your config directory. A project can override the safe behavioural fields with its own .opencli/config.json.
            </p>
            <dl className="mt-7 border-t-2 border-ink">
              {configFields.map((f) => (
                <div key={f.key} className="grid grid-cols-[10rem_1fr] gap-4 border-b border-line py-3">
                  <dt className="font-mono text-[13px] font-medium text-ink">{f.key}</dt>
                  <dd className="text-[14px] leading-snug text-ink-2">{f.desc}</dd>
                </div>
              ))}
            </dl>
          </div>
          <div className="lg:pt-2">
            <CodeBlock label="config.json" code={configJson} />
            <p className="mt-4 text-[13.5px] leading-relaxed text-ink-2">
              Security-sensitive keys like default_permission_mode, the auto-approve flags, and providers are global-only, so a cloned repo cannot disable approvals or redirect the model.
            </p>
          </div>
        </div>
      </section>

      {/* Build from source. */}
      <section>
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <div className="grid gap-10 lg:grid-cols-[1fr_1.1fr] lg:gap-16">
            <div>
              <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
                Build from source
              </h2>
              <p className="mt-4 text-[1.0625rem] leading-relaxed text-ink-2">
                You need stable Rust, and ripgrep is recommended since it powers the grep tool. Then link it into your PATH, or run it in dev mode.
              </p>
              <div className="mt-8 flex flex-wrap gap-3">
                <Link
                  href="/field-guide"
                  className="inline-flex items-center gap-2 bg-ink px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-bg transition-colors hover:bg-ink-2"
                >
                  Read the field guide
                  <ArrowRight size={15} weight="bold" />
                </Link>
                <a
                  href={site.contributingUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-2 border border-ink px-7 py-3.5 font-mono text-[12.5px] uppercase tracking-[0.14em] text-ink transition-colors hover:bg-ink hover:text-bg"
                >
                  Contributing
                </a>
              </div>
            </div>
            <div className="lg:pt-2">
              <CodeBlock
                label="from source"
                code={`git clone ${site.repoUrl} && cd opencli\nmake install      # build release + link to PATH\nmake link-dev     # OR dev mode, re-runs cargo on each call\nmake unlink       # remove the link`}
              />
            </div>
          </div>
        </div>
      </section>
    </>
  );
}
