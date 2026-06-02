import type { Metadata } from "next";
import { ArrowRight } from "@phosphor-icons/react/dist/ssr";
import { site, providers, models, reasoningLevels } from "@/lib/content";
import { PageHeader } from "@/components/PageHeader";
import { CodeBlock } from "@/components/CodeBlock";
import { Reveal } from "@/components/Reveal";

export const metadata: Metadata = {
  title: "Models",
  description:
    "Providers and models OpenCLI supports: the OpenAI GPT-5 family, the Anthropic Claude 4 family, and any OpenAI-compatible endpoint.",
};

const ACCENT_HEX: Record<string, string> = {
  oai: "#0a7d6b",
  ant: "#c0552d",
  compat: "#2f53d6",
};

const providersConfig = `{
  "providers": {
    "groq": {
      "base_url": "https://api.groq.com/openai/v1",
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
        title="Point it at any provider."
        intro="OpenCLI is provider-agnostic. Sign in with a subscription or an API key, switch models mid-session, and add any OpenAI-compatible endpoint. This catalogue reflects the latest release."
      />

      {/* Providers. */}
      <section className="border-b border-line">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <div className="border-t-2 border-ink">
            {providers.map((p) => (
              <div
                key={p.key}
                className="grid gap-3 border-b border-line py-7 sm:grid-cols-[0.8fr_1.2fr] sm:gap-8"
              >
                <div className="flex items-start gap-3">
                  <span
                    className="mt-2 inline-block h-3.5 w-3.5 shrink-0"
                    style={{ background: ACCENT_HEX[p.accent] }}
                  />
                  <div>
                    <h2 className="font-display text-[1.8rem] font-bold leading-none text-ink">
                      {p.name}
                    </h2>
                    <p className="mt-2 font-mono text-[12px] text-ink-3">{p.signIn}</p>
                  </div>
                </div>
                <p className="text-[15px] leading-relaxed text-ink-2">{p.body}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Catalogue. */}
      <section className="border-b border-line bg-bg-2">
        <div className="mx-auto max-w-[1200px] px-5 py-16 sm:px-8 sm:py-24">
          <h2 className="font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
            The current catalogue
          </h2>
          <p className="mt-4 max-w-2xl text-[1.0625rem] leading-relaxed text-ink-2">
            Context windows are approximate. Retired ids auto-migrate to their current equivalent on startup, so an existing config keeps working across releases.
          </p>

          <div className="mt-10 grid gap-x-14 gap-y-12 lg:grid-cols-2">
            {byProvider.map((group) => (
              <div key={group.name}>
                <div className="flex items-center justify-between border-b-2 border-ink pb-2">
                  <div className="flex items-center gap-2.5">
                    <span className="inline-block h-3 w-3" style={{ background: group.hex }} />
                    <h3 className="font-display text-[1.4rem] font-bold text-ink">{group.name}</h3>
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
                          <p className="mt-1 text-[13.5px] leading-snug text-ink-2">{m.note}</p>
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
            <h2 className="font-display text-[1.9rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[2.3rem]">
              Reasoning levels
            </h2>
            <p className="mt-3 text-[15px] leading-relaxed text-ink-2">
              The same effort scale works across providers. The newest Claude models think adaptively at the top of the range.
            </p>
            <div className="mt-6 flex flex-wrap gap-2">
              {reasoningLevels.map((level) => (
                <span
                  key={level}
                  className="border border-line-2 bg-bg px-4 py-2 font-mono text-[13px] text-ink"
                >
                  {level}
                </span>
              ))}
            </div>
          </div>
          <div>
            <h2 className="font-display text-[1.9rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[2.3rem]">
              OpenAI-compatible endpoints
            </h2>
            <p className="mt-3 text-[15px] leading-relaxed text-ink-2">
              Declare a base URL, key, and context limit under providers in config.json, then address the model as provider/model.
            </p>
            <CodeBlock label="config.json" code={providersConfig} className="mt-5" />
          </div>
        </div>
      </section>

      {/* Source note. */}
      <section>
        <div className="mx-auto max-w-[1200px] px-5 py-14 sm:px-8">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <p className="max-w-xl text-[14.5px] leading-relaxed text-ink-2">
              Model availability changes between releases. The catalogue in the binary is always the source of truth.
            </p>
            <a
              href={site.repoUrl}
              target="_blank"
              rel="noreferrer"
              className="inline-flex shrink-0 items-center gap-2 font-mono text-[12.5px] uppercase tracking-[0.12em] text-ink transition-colors hover:text-ink-2"
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
