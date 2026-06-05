"use client";

import { useState } from "react";
import { decisionTrail, trailModels } from "@/lib/content";

/** Provider accents, reused from the Swiss palette so model identity carries color. */
const HEX: Record<string, string> = { oai: "#0a7d6b", ant: "#c0552d" };
const accentHex = (id: string) =>
  HEX[trailModels.find((m) => m.id === id)?.accent ?? "oai"];

/**
 * The moat, made interactive. Pick a location to see the recorded reasoning and
 * the alternatives that were rejected. Flip "model in play" to a different
 * vendor: the decisions do not change, and the footer shows the new model
 * inheriting the original's reasoning verbatim. That is the cross-model trail.
 */
export function DecisionTrail() {
  const [loc, setLoc] = useState(decisionTrail[0].loc);
  const [model, setModel] = useState(trailModels[0].id);
  const d = decisionTrail.find((x) => x.loc === loc) ?? decisionTrail[0];
  const switched = model !== d.model;

  return (
    <div>
      <p className="mono-label">Pillar 02 / the decision trail</p>
      <div className="mt-4 flex flex-col gap-5 lg:flex-row lg:items-end lg:justify-between">
        <h2 className="max-w-2xl font-display text-[2.2rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
          It remembers why.
          <br />
          Even after you switch models.
        </h2>
        <p className="max-w-sm text-[1.0625rem] leading-relaxed text-ink-2">
          Tomte records the reasoning behind each change, not just the diff. Switch providers mid-task and the new model inherits the why, not a lossy summary.
        </p>
      </div>

      {/* The console: a real `tomte why` read, made clickable. */}
      <div className="mt-10 border border-ink bg-bg">
        {/* Top bar: the command, and the model in play. */}
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-line-2 bg-bg-2 px-4 py-3">
          <span className="font-mono text-[12.5px] text-ink-2">
            <span className="text-ink-3">$ </span>
            tomte why {d.loc}
          </span>
          <div className="flex items-center gap-2.5">
            <span className="mono-label">model in play</span>
            <div className="flex border border-line-2">
              {trailModels.map((m, i) => {
                const on = m.id === model;
                return (
                  <button
                    key={m.id}
                    type="button"
                    onClick={() => setModel(m.id)}
                    aria-pressed={on}
                    className={`px-2.5 py-1 font-mono text-[11px] transition-colors ${
                      i > 0 ? "border-l border-line-2" : ""
                    } ${on ? "text-bg" : "text-ink-2 hover:text-ink"}`}
                    style={{ background: on ? HEX[m.accent] : "transparent" }}
                  >
                    {m.id}
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        {/* Body: the trail on the left, the active decision on the right. */}
        <div className="grid sm:grid-cols-[0.85fr_1.15fr]">
          <div className="border-b border-line-2 sm:border-b-0 sm:border-r">
            {decisionTrail.map((x, i) => {
              const on = x.loc === loc;
              return (
                <button
                  key={x.loc}
                  type="button"
                  onClick={() => setLoc(x.loc)}
                  aria-pressed={on}
                  className={`flex w-full items-center justify-between gap-3 px-4 py-3 text-left transition-colors ${
                    i > 0 ? "border-t border-line" : ""
                  } ${on ? "bg-bg-2" : "hover:bg-bg-2"}`}
                >
                  <span className="font-mono text-[12px] text-ink">{x.loc}</span>
                  <span
                    className="inline-block h-2 w-2 shrink-0"
                    style={{ background: accentHex(x.model) }}
                    aria-hidden="true"
                  />
                </button>
              );
            })}
          </div>

          <div className="px-5 py-5">
            <p className="font-display text-[1.25rem] font-bold leading-snug text-ink">
              {d.decision}
            </p>
            <p className="mt-3 text-[14.5px] leading-relaxed text-ink-2">{d.why}</p>

            <p className="mono-label mt-5">rejected</p>
            <ul className="mt-2 space-y-1">
              {d.rejected.map((r) => (
                <li key={r} className="font-mono text-[12.5px] text-ink-3 line-through">
                  {r}
                </li>
              ))}
            </ul>

            <p className="mt-5 font-mono text-[11.5px] text-ink-3">
              recorded turn {d.turn} by{" "}
              <span style={{ color: accentHex(d.model) }}>{d.model}</span>
            </p>
          </div>
        </div>

        {/* Footer: the cross-model claim, proven by the toggle above. */}
        <div
          className="border-t border-line-2 px-5 py-3 text-[13px] leading-relaxed"
          style={{ background: switched ? `${accentHex(model)}10` : "transparent" }}
        >
          {switched ? (
            <span className="text-ink-2">
              <span style={{ color: accentHex(model) }}>{model}</span> inherits{" "}
              <span style={{ color: accentHex(d.model) }}>{d.model}</span>
              {"'"}s reasoning, verbatim. The why survived the switch.
            </span>
          ) : (
            <span className="text-ink-3">
              Recorded by {d.model}. Switch the model above and the reasoning stays put.
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
