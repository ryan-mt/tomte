import type { Metadata } from "next";
import { ArrowRight } from "@phosphor-icons/react/dist/ssr";
import { site, providers, models, reasoningLevels } from "@/lib/content";
import { PageHeader } from "@/components/PageHeader";
import { CodeBlock } from "@/components/CodeBlock";
import { Reveal } from "@/components/Reveal";

export const metadata: Metadata = {
  title: "Models",
  description:
    "Providers and models tomte supports: the OpenAI GPT-5 family, Anthropic's Claude Fable 5 and Claude 4 families, and any OpenAI-compatible endpoint.",
};

const ACCENT_HEX: Record<string, string> = {
  oai: "#5fb8a3",
  ant: "#d98a62",
  compat: "#7d9bd6",
};

const providersConfig = `# built-in presets work out of the box:
tomte config --set-model groq/llama-3.3-70b
# anything else: declare it in config.json
{
  "providers": {
    "myhost": {
      "base_url": "https://api.myhost.dev/v1",
      "api_key": "...",
      "context_limit": 131072
    }
  }
}`;

const byProvider = [
  { name: "OpenAI", hex: ACCENT_HEX.oai, rows: models.filter((m) => m.provider === "OpenAI") },
  { name: "Anthropic", hex: ACCENT_HEX.ant, rows: models.filter((m) => m.provider === "Anthropic") },
];

export default function Models() {
  return (
    <>
      <PageHeader
        kicker="Reference"
        title="Point it at any brain."
        intro="tomte is provider-agnostic. Sign in with a subscription or an API key, switch models mid-session, and add any OpenAI-compatible endpoint. The trail, the map, and the proofs survive the switch. This catalogue reflects the latest release."
      />

      {/* Providers. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <div className="border-t border-line-2">
            {providers.map((p) => (
              <div
                key={p.key}
                className="grid gap-3 border-b border-line py-7 sm:grid-cols-[0.8fr_1.2fr] sm:gap-8"
              >
                <div className="flex items-start gap-3">
                  <span
                    className="mt-2 inline-block size-3 shrink-0 rounded-full"
                    style={{ background: ACCENT_HEX[p.accent] }}
                  />
                  <div>
                    <h2 className="text-[1.7rem] leading-none">{p.name}</h2>
                    <p className="mt-2 font-mono text-[12px] text-ink-3">{p.signIn}</p>
                  </div>
                </div>
                <p className="text-[14.5px] leading-relaxed text-ink-2">{p.body}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Catalogue. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="text-[2rem] sm:text-[2.6rem]">The current catalogue.</h2>
          <p className="mt-4 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
            Context windows are approximate. Retired ids auto-migrate to their
            current equivalent on startup, so an existing config keeps working
            across releases.
          </p>

          <div className="mt-10 grid gap-x-14 gap-y-12 lg:grid-cols-2">
            {byProvider.map((group) => (
              <div key={group.name}>
                <div className="flex items-center justify-between border-b border-line-2 pb-2">
                  <div className="flex items-center gap-2.5">
                    <span className="inline-block size-2.5 rounded-full" style={{ background: group.hex }} />
                    <h3 className="text-[1.35rem]">{group.name}</h3>
                  </div>
                  <span className="font-mono text-[11px] uppercase tracking-[0.14em] text-ink-3">
                    {group.rows.length} models
                  </span>
                </div>
                <div>
                  {group.rows.map((m) => (
                    <Reveal key={m.id}>
                      <div className="grid grid-cols-[1fr_auto] items-baseline gap-3 border-b border-line py-3">
                        <div>
                          <span className="font-mono text-[14px] text-ink">{m.id}</span>
                          <p className="mt-1 text-[13px] leading-snug text-ink-2">{m.note}</p>
                        </div>
                        <span className="font-mono text-[12px] text-ink-3">{m.context}</span>
                      </div>
                    </Reveal>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Reasoning + compatible. */}
      <section className="border-b border-line">
        <div className="mx-auto grid max-w-[1200px] gap-12 px-5 py-16 sm:px-8 sm:py-24 lg:grid-cols-2 lg:gap-16">
          <div>
            <h2 className="text-[1.8rem] sm:text-[2.2rem]">Reasoning levels.</h2>
            <p className="mt-3 text-[14.5px] leading-relaxed text-ink-2">
              The same effort scale works across providers. The newest Claude
              models think adaptively, and Fable honours the top of the range
              instead of clamping it.
            </p>
            <div className="mt-6 flex flex-wrap gap-2">
              {reasoningLevels.map((level) => (
                <span
                  key={level}
                  className="rounded-md border border-line-2 bg-bg-2 px-4 py-2 font-mono text-[13px] text-ink"
                >
                  {level}
                </span>
              ))}
            </div>
          </div>
          <div>
            <h2 className="text-[1.8rem] sm:text-[2.2rem]">
              OpenAI-compatible endpoints.
            </h2>
            <p className="mt-3 text-[14.5px] leading-relaxed text-ink-2">
              Ten presets work out of the box as provider/model, local servers
              need no key, and anything else takes a base URL in config.json.
            </p>
            <CodeBlock label="presets + config.json" code={providersConfig} className="mt-5" />
          </div>
        </div>
      </section>

      {/* Source note. */}
      <section>
        <div className="mx-auto max-w-[1200px] px-5 py-14 sm:px-8">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <p className="max-w-xl text-[14px] leading-relaxed text-ink-2">
              Model availability changes between releases. The catalogue in the
              binary is always the source of truth.
            </p>
            <a
              href={site.repoUrl}
              target="_blank"
              rel="noreferrer"
              className="inline-flex shrink-0 items-center gap-2 font-mono text-[12.5px] uppercase tracking-[0.12em] text-hearth transition-colors hover:text-ink"
            >
              Read the source
              <ArrowRight size={14} weight="bold" />
            </a>
          </div>
        </div>
      </section>
    </>
  );
}
