"use client";

import { useState } from "react";
import { motion, useReducedMotion } from "motion/react";
import { providers, models } from "@/lib/content";

const HEX: Record<string, string> = {
  oai: "#0a7d6b",
  ant: "#c0552d",
  compat: "#2f53d6",
};

// vertical centre of each provider node inside the 400x320 viewBox
const NODE_Y: Record<string, number> = {
  openai: 58,
  anthropic: 160,
  compatible: 262,
};

const SHORT: Record<string, string> = {
  openai: "OPENAI",
  anthropic: "ANTHROPIC",
  compatible: "CUSTOM",
};

function countFor(key: string): number | null {
  if (key === "openai") return models.filter((m) => m.provider === "OpenAI").length;
  if (key === "anthropic") return models.filter((m) => m.provider === "Anthropic").length;
  return null;
}

export function Switchboard() {
  const reduce = useReducedMotion();
  const [active, setActive] = useState(providers[0].key);
  const current = providers.find((p) => p.key === active) ?? providers[0];
  const accent = HEX[current.accent];

  return (
    <div className="grid items-center gap-10 lg:grid-cols-[0.92fr_1.08fr] lg:gap-14">
      <div>
        <h2 className="font-display text-[2.4rem] font-extrabold leading-[0.95] tracking-[-0.03em] text-ink sm:text-[3rem]">
          One binary.
          <br />
          Any model.
        </h2>
        <p className="mt-5 max-w-md text-[1.0625rem] leading-relaxed text-ink-2">
          Tomte routes a single agent to whichever provider you point it at. Switch models mid-session, or fail over automatically when one is rate-limited.
        </p>

        <div className="mt-7 border border-ink">
          {providers.map((p, i) => {
            const on = p.key === active;
            const c = HEX[p.accent];
            const n = countFor(p.key);
            return (
              <button
                key={p.key}
                type="button"
                onClick={() => setActive(p.key)}
                onMouseEnter={() => setActive(p.key)}
                onFocus={() => setActive(p.key)}
                aria-pressed={on}
                className={`flex w-full items-center justify-between gap-4 px-4 py-3.5 text-left transition-colors ${
                  i > 0 ? "border-t border-line-2" : ""
                }`}
                style={{ background: on ? `${c}14` : "transparent" }}
              >
                <span className="flex items-center gap-3">
                  <span className="inline-block h-3 w-3" style={{ background: c }} />
                  <span className="font-display text-[1.15rem] font-bold text-ink">
                    {p.name}
                  </span>
                </span>
                <span className="font-mono text-[11px] uppercase tracking-[0.12em] text-ink-3">
                  {n !== null ? `${n} models` : p.tag}
                </span>
              </button>
            );
          })}
        </div>

        <p className="mt-4 flex items-center gap-2 font-mono text-[12px] text-ink-2">
          <span className="inline-block h-2 w-2" style={{ background: accent }} />
          {current.signIn}
        </p>
      </div>

      <div className="blueprint relative border border-line-2 bg-bg-2 p-4">
        <span className="mono-label absolute right-3.5 top-3.5">routing</span>
        <svg viewBox="0 0 400 320" className="w-full" role="img" aria-label="Tomte routing a single core to multiple providers">
          {providers.map((p, i) => {
            const on = p.key === active;
            const y = NODE_Y[p.key];
            const d = `M150 160 C 222 160, 232 ${y}, 300 ${y}`;
            return (
              <motion.path
                key={p.key}
                d={d}
                fill="none"
                stroke={on ? HEX[p.accent] : "#c6c6bb"}
                strokeWidth={on ? 2.5 : 1}
                initial={reduce ? false : { pathLength: 0, opacity: 0 }}
                animate={{ pathLength: 1, opacity: on ? 1 : 0.45 }}
                transition={{
                  pathLength: { duration: reduce ? 0 : 0.7, delay: reduce ? 0 : 0.12 * i, ease: [0.16, 1, 0.3, 1] },
                  stroke: { duration: 0.25 },
                  strokeWidth: { duration: 0.25 },
                  opacity: { duration: 0.25 },
                }}
              />
            );
          })}

          <rect x="40" y="134" width="110" height="52" fill="#f4f4f1" stroke={accent} strokeWidth="2" />
          <text x="95" y="165" textAnchor="middle" className="font-mono" fontSize="15" fontWeight="700" fill="#131312">
            tomte
          </text>

          {providers.map((p) => {
            const on = p.key === active;
            const y = NODE_Y[p.key];
            const c = HEX[p.accent];
            return (
              <g
                key={p.key}
                aria-hidden="true"
                style={{ cursor: "pointer" }}
                onClick={() => setActive(p.key)}
                onMouseEnter={() => setActive(p.key)}
              >
                <rect x="300" y={y - 19} width="94" height="38" fill={on ? c : "#f4f4f1"} stroke={on ? c : "#c6c6bb"} strokeWidth="1" />
                <text x="347" y={y + 4} textAnchor="middle" className="font-mono" fontSize="10" fontWeight="600" letterSpacing="1" fill={on ? "#f4f4f1" : "#56564e"}>
                  {SHORT[p.key]}
                </text>
              </g>
            );
          })}
        </svg>
      </div>
    </div>
  );
}
