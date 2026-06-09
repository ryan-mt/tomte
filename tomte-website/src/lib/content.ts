/**
 * content.ts: single source of truth for tomte's website.
 *
 * STABILITY CONTRACT: everything that changes between releases (version,
 * model catalogue, tool belt, slash commands, links) lives HERE and nowhere
 * else. To update the site for a new release, edit this file only. Page
 * components read from these exports and never hard-code volatile values.
 *
 * Copy style: no em-dashes anywhere (regular hyphen only), concrete language,
 * English throughout. Every claim must trace to a shipped feature.
 */

export const site = {
  name: "tomte",
  /** Wordmark as shown in the masthead. */
  wordmark: "tomte",
  headline: "The coding agent that proves its work.",
  /** Hero subtext: short, concrete, no em-dash. */
  subhead:
    "A calm, multi-model agent for your terminal. It maps the repository, remembers why, and never calls a thing done without evidence.",
  /** Longer one-paragraph description for meta + intro plates. */
  description:
    "tomte is a single-binary coding agent for your terminal, named for the Nordic farm spirit who keeps the household in order overnight. Point it at OpenAI or Anthropic and it reads, writes, runs, and reasons through real work. What no other terminal agent ships together: a proof capsule built from your project's own checks, a decision trail that survives model switches, a verifiable map of the repository, and an agent tournament with a deterministic judge.",
  license: "MIT",
  language: "Rust",
  /** Canonical version; the UI prefers linking to the latest release. */
  version: "0.0.3",
  repoUrl: "https://github.com/ryan-mt/tomte",
  releasesUrl: "https://github.com/ryan-mt/tomte/releases",
  latestReleaseUrl: "https://github.com/ryan-mt/tomte/releases/latest",
  contributingUrl: "https://github.com/ryan-mt/tomte/blob/main/CONTRIBUTING.md",
  licenseUrl: "https://github.com/ryan-mt/tomte/blob/main/LICENSE",
} as const;

export type NavItem = { label: string; href: string };

export const nav: NavItem[] = [
  { label: "Overview", href: "/" },
  { label: "Field guide", href: "/field-guide" },
  { label: "Models", href: "/models" },
  { label: "Install", href: "/install" },
];

/**
 * The four proofs: what only tomte ships together. Each card carries a REAL
 * command and a REAL excerpt of its output (trimmed), so the site eats the
 * same food the terminal serves. Update excerpts when output formats change.
 */
export type Proof = {
  /** stable key for layout */
  key: "prove" | "why" | "twin" | "race";
  /** rubber-stamp label on the capsule */
  stamp: string;
  title: string;
  body: string;
  command: string;
  /** trimmed, real output lines */
  excerpt: string[];
  /** one-line "why it cannot lie" note */
  honest: string;
};

export const proofs: Proof[] = [
  {
    key: "prove",
    stamp: "verified",
    title: "Done means verified",
    body: "/prove collects an evidence bundle the CLI gathers itself: the files git reports changed, plus the real exit codes of your project's own test, typecheck, lint, and build. tomte prove exits non-zero on failure, so it gates a commit hook or CI step.",
    command: "tomte prove",
    excerpt: [
      "Proof Capsule  ·  2026-06-09 10:58",
      "  files changed   1 (M README.md)",
      "  ✅ test       passed   cargo test",
      "  ✅ typecheck  passed   cargo check",
      "  ✅ lint       passed   cargo clippy",
      "  ✅ build      passed   cargo build",
      "  reproduce: cargo test && cargo clippy",
    ],
    honest:
      "The model never supplies these numbers. It cannot fabricate a green capsule, only explain one the CLI already collected.",
  },
  {
    key: "why",
    stamp: "on record",
    title: "It remembers why, across models",
    body: "record_decision appends the reasoning behind every non-obvious change to a decision trail that is re-injected each session. Next month's session, or a different model entirely, inherits the why and not just the diff. Drift Watch flags a decision the code has moved out from under.",
    command: "tomte why src/parser.rs:88",
    excerpt: [
      "src/parser.rs:88",
      "  decision  empty input returns Err, not a panic",
      "  why       a library must never crash its caller",
      "  rejected  panic: crashes callers",
      "  recorded  gpt-5.5 · turn 5 · anchor fresh",
    ],
    honest:
      "The trail is an append-only file in your project state. Overturning a decision is recorded as a supersede, never an erase.",
  },
  {
    key: "twin",
    stamp: "mapped",
    title: "It knows the house",
    body: "tomte twin builds five verifiable indexes straight from the source: import graph, symbol graph, test-to-source map, git recency, and project conventions. tomte why-context answers the question context-stuffing agents dodge: which files actually belong in context, and why.",
    command: "tomte why-context classify_danger",
    excerpt: [
      "Context X-Ray for `classify_danger`",
      "Selected (would pull into context):",
      "  • tools/shell.rs",
      "      because imports the seed [import]",
      "  • race/judge.rs",
      "      because judge.rs:6 references it [symbol]",
      "Ignored (nearby but left out):",
      "  • tools/web.rs — no path reaches it",
    ],
    honest:
      "Every claim is grounded in a real import edge, definition, test, or commit. A generic name cannot manufacture a false reference.",
  },
  {
    key: "race",
    stamp: "measured",
    title: "Don't trust one agent. Race them",
    body: "tomte race runs a task as a tournament: contestants varying model, effort, and style, each in its own isolated git worktree. The judge is deterministic and measures evidence: the project's own checks, diff size, added tests, risky commands run. An LLM is never the referee.",
    command: 'tomte race "fix the flaky retry test" --agents 4',
    excerpt: [
      "🏁 4 contestants · isolated worktrees",
      "  1. minimal-patch   ✅ verified · +test · 38 lines",
      "  2. gpt-5.5/high    ✅ verified · 112 lines",
      "  3. opus-4-8/max    ⚠ checks failed (lint)",
      "  4. gpt-5.5/low     ✖ no change",
      "  winner: minimal-patch (smallest verified diff)",
      "  patch saved · apply with --apply",
    ],
    honest:
      "Ranking is tiered so a clever-but-broken patch can never beat a working one. Every reason on the card comes from measured numbers.",
  },
];

/**
 * The composed vitals: what the indexes unlock when they are real data.
 * Pulse and Handoff render from the same twin the X-ray uses.
 */
export type Vital = {
  key: "pulse" | "handoff";
  title: string;
  body: string;
  command: string;
  excerpt: string[];
};

export const vitals: Vital[] = [
  {
    key: "pulse",
    title: "Repo Pulse",
    body: "Which files are most likely to break next, with the formula printed on the card: commits in the recent window, times import fan-in plus one, doubled when no test covers the file. Rerun it, get the same card, argue with the numbers.",
    command: "tomte pulse",
    excerpt: [
      "Repo Pulse — your/repo",
      "  1. core/src/tools/mod.rs",
      "     risk 124 = 31c × 2i × untested ⚠",
      "  2. tui/app/types.rs",
      "     risk 76 = 19c × 2i × untested ⚠",
      "  hot & untested: 65 source files",
    ],
  },
  {
    key: "handoff",
    title: "The Handoff capsule",
    body: "One paste-ready markdown capsule: git standing, the newest recorded decisions with a drift-watch line, the map summary, and the pulse top. Built for the next session, whether that is a colleague, tomorrow's you, or a different model entirely.",
    command: "tomte handoff --out HANDOFF.md",
    excerpt: [
      "# Handoff — your/repo",
      "## Where the tree stands",
      "- branch `0.0.3` · working tree clean",
      "## Why things are the way they are",
      "- `parser.rs:88` — Err, not panic",
      "- drift watch: 4 hold · 2 healed · 1 needs eyes",
      "_Before you call anything done: tomte prove._",
    ],
  },
];

/** The keeper's manner: the quieter habits wrapped around the four proofs. */
export const manners: { title: string; body: string }[] = [
  {
    title: "Glass box, not black box",
    body: "Before a write or shell command runs, one calm line states what it changes and how far it reaches. A file's recorded decisions surface as house rules so the agent re-reads its own constraints before it could break one.",
  },
  {
    title: "An end-of-turn receipt",
    body: "A turn that changes something closes with one line: files touched, tests run, and the why it recorded. The custodian leaves a note, every time.",
  },
  {
    title: "A checkpoint every turn",
    body: "/undo reverts the last file edit. /rewind restores the session to an earlier turn AND reverts the edits made since, each picker row showing its blast radius before you commit to it.",
  },
  {
    title: "Quiet, surgical, cross-platform",
    body: "One Rust binary on Linux, macOS, and Windows. No daemon, no telemetry, a terminal UI that stays out of the way, and a pixel companion that hatches from an egg if you want company.",
  },
];

/** Headline capabilities (the table-stakes done well). */
export type Capability = {
  tag: string;
  title: string;
  body: string;
};

export const capabilities: Capability[] = [
  {
    tag: "one binary",
    title: "No daemon, no ceremony",
    body: "A single tomte binary. Launch the full terminal UI or fire a one-shot from a script. Same agent either way, nothing running in the background.",
  },
  {
    tag: "your brain",
    title: "Bring your own provider",
    body: "Sign in with a ChatGPT or Claude subscription over OAuth, or paste an API key. Switch models mid-session. Add any OpenAI-compatible endpoint, local or hosted.",
  },
  {
    tag: "tool belt",
    title: "A real tool belt, not a toy",
    body: "Twenty-seven tools across files, shell, search, web, notebooks, sub-agents, memory, todos, and plan mode. Streamed, schema-validated, and run in parallel where it is safe.",
  },
  {
    tag: "lsp",
    title: "Code intelligence, zero setup",
    body: "The lsp tool gives symbols, go-to-definition, references, and hover for Rust, TypeScript, JavaScript, Python, and Go. No language server to install.",
  },
  {
    tag: "worktree",
    title: "Experiment without fear",
    body: "enter_worktree spins the session into an isolated git worktree. exit_worktree cleans it up after a safety check, so you never clobber main.",
  },
  {
    tag: "accounting",
    title: "Knows what it is spending",
    body: "/usage reads your provider's live quota. /cost tallies tokens and dollars per model, cache-aware. /context shows where the window is going.",
  },
  {
    tag: "failover",
    title: "Stays up when a provider does not",
    body: "List fallback models and a rate-limit or overload transparently switches the turn to the next one and keeps going, instead of failing mid-task. Off by default.",
  },
  {
    tag: "memory",
    title: "Inherits your existing setup",
    body: "AGENTS.md and CLAUDE.md from the git root down to your working directory fold into the system prompt. Existing skills and sub-agents are discovered automatically.",
  },
];

/**
 * Decision-trail demo data for the home page. Mirrors the real record shape.
 */
export type Decision = {
  loc: string;
  decision: string;
  why: string;
  rejected: string[];
  model: string;
  turn: number;
};

export const decisionTrail: Decision[] = [
  {
    loc: "src/api/auth.rs:15",
    decision: "Verify the JWT signature before decoding any claims",
    why: "Claims are attacker-controlled. Trusting them before verification is the bug class behind most JWT CVEs.",
    rejected: [
      "decode then verify: opens a TOCTOU window",
      "skip the exp check: tokens would never expire",
    ],
    model: "gpt-5.5",
    turn: 2,
  },
  {
    loc: "src/cache.rs:42",
    decision: "Bound the cache with LRU eviction at 1024 entries",
    why: "Unbounded growth runs the process out of memory under load. Profiling holds the hit rate above 90% at 1024.",
    rejected: [
      "unbounded HashMap: out of memory under load",
      "TTL only: cold keys still pin memory",
    ],
    model: "gpt-5.5",
    turn: 4,
  },
  {
    loc: "src/parser.rs:88",
    decision: "Empty input returns Err, not a panic",
    why: "The parser validates at the boundary. A library must never crash its caller.",
    rejected: ["panic: crashes callers", "Ok(0): silently hides the error"],
    model: "gpt-5.5",
    turn: 5,
  },
];

/** Models offered in the trail demo's "model in play" toggle. */
export const trailModels: { id: string; accent: "oai" | "ant" }[] = [
  { id: "gpt-5.5", accent: "oai" },
  { id: "claude-fable-5", accent: "ant" },
];

/** The tool belt, grouped exactly as the agent exposes it. 27 tools total. */
export type ToolGroup = { group: string; blurb: string; tools: string[] };

export const toolBelt: ToolGroup[] = [
  {
    group: "Files",
    blurb: "Read and edit with stale-file guards that refuse a write when a file changed since it was last read, plus a one-step undo.",
    tools: ["read_file", "write_file", "edit_file", "multi_edit", "undo_last_edit", "list_dir"],
  },
  {
    group: "Search",
    blurb: "Regex search, glob, and language-aware code intelligence without setup.",
    tools: ["grep", "glob", "lsp"],
  },
  {
    group: "Shell",
    blurb: "Run commands with a destructive-command guard, plus background shells you can poll and kill.",
    tools: ["run_shell", "bash_output", "kill_shell"],
  },
  {
    group: "Web",
    blurb: "Fetch and search the web behind an SSRF guard with a response-size cap.",
    tools: ["web_fetch", "web_search"],
  },
  {
    group: "Flow",
    blurb: "Track todos with dependencies, hold an active goal, wait, and move in and out of plan mode.",
    tools: ["todo_write", "goal_update", "enter_plan_mode", "exit_plan_mode", "wait"],
  },
  {
    group: "Agents",
    blurb: "Dispatch sub-agents, ask the user, and invoke skills.",
    tools: ["dispatch_agent", "ask_user_question", "skill"],
  },
  {
    group: "Memory",
    blurb: "Record why a non-obvious change was made, then read it back across sessions and model switches, plus project-scoped notes that persist.",
    tools: ["memory", "record_decision"],
  },
  {
    group: "Git worktrees",
    blurb: "Branch the session into a throwaway worktree and clean it up safely.",
    tools: ["enter_worktree", "exit_worktree"],
  },
  {
    group: "Notebooks",
    blurb: "Edit Jupyter notebook cells with the same stale-file guard as files.",
    tools: ["notebook_edit"],
  },
];

export const toolCount = toolBelt.reduce((n, g) => n + g.tools.length, 0);

/** Providers. Phrased as families so the page survives model churn. */
export type Provider = {
  key: string;
  name: string;
  accent: "oai" | "ant" | "compat";
  tag: string;
  signIn: string;
  body: string;
};

export const providers: Provider[] = [
  {
    key: "openai",
    name: "OpenAI",
    accent: "oai",
    tag: "GPT-5 family",
    signIn: "ChatGPT subscription (OAuth) or API key",
    body: "The GPT-5 family over the Responses and Chat Completions APIs. A ChatGPT Plus, Pro, Team, or Enterprise subscription signs in over OAuth. An API key unlocks the full public catalogue.",
  },
  {
    key: "anthropic",
    name: "Anthropic",
    accent: "ant",
    tag: "Claude families",
    signIn: "Claude subscription (OAuth) or API key",
    body: "Claude Fable 5 and the Claude 4 family over the Messages API, with adaptive thinking on the newest models. A Claude Pro or Max subscription signs in over OAuth after a terms acknowledgement.",
  },
  {
    key: "compatible",
    name: "OpenAI-compatible",
    accent: "compat",
    tag: "Any endpoint",
    signIn: "Built-in presets or config.json",
    body: "Groq, OpenRouter, DeepSeek, xAI, Together, Fireworks, Cerebras, Mistral, and local Ollama or LM Studio work out of the box as provider/model. Anything else: declare a base URL and key under providers in config.json.",
  },
];

/** Model catalogue. Update this list per release; the page renders from it. */
export type ModelRow = {
  id: string;
  provider: "OpenAI" | "Anthropic";
  context: string;
  note: string;
};

export const models: ModelRow[] = [
  { id: "gpt-5.5", provider: "OpenAI", context: "1.05M", note: "Default. Largest OpenAI context window." },
  { id: "gpt-5.5-pro", provider: "OpenAI", context: "1.05M", note: "Extended reasoning for hard agent tasks." },
  { id: "gpt-5.4", provider: "OpenAI", context: "1M", note: "Previous frontier, stable." },
  { id: "gpt-5.4-mini", provider: "OpenAI", context: "400K", note: "Fast and cheaper, strong for routine code." },
  { id: "gpt-5.4-nano", provider: "OpenAI", context: "200K", note: "Latency-sensitive, cheapest." },
  { id: "gpt-5.2", provider: "OpenAI", context: "400K", note: "Earlier frontier, still selectable." },
  { id: "gpt-5", provider: "OpenAI", context: "400K", note: "Earlier frontier, still selectable." },
  { id: "claude-fable-5", provider: "Anthropic", context: "1M", note: "Top tier. Adaptive thinking, xhigh effort honoured." },
  { id: "claude-opus-4-8", provider: "Anthropic", context: "1M", note: "Frontier Opus. Adaptive extended thinking." },
  { id: "claude-opus-4-7", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-opus-4-6", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-opus-4-5", provider: "Anthropic", context: "200K", note: "Prior Opus generation." },
  { id: "claude-sonnet-4-6", provider: "Anthropic", context: "1M", note: "Balanced speed and capability." },
  { id: "claude-sonnet-4-5", provider: "Anthropic", context: "200K", note: "Prior Sonnet generation." },
  { id: "claude-haiku-4-5", provider: "Anthropic", context: "200K", note: "Fastest, lowest cost." },
];

/** Reasoning effort levels the agent understands. */
export const reasoningLevels = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;

/** Curated slash commands worth knowing. Grouped for a card-per-group layout. */
export type CommandGroup = { group: string; items: { cmd: string; desc: string }[] };

export const slashCommands: CommandGroup[] = [
  {
    group: "Evidence",
    items: [
      { cmd: "/prove", desc: "Run the project's own checks and show the proof capsule." },
      { cmd: "/twin", desc: "The repo's five verifiable indexes, built and cached." },
      { cmd: "/why-context <seed>", desc: "Which files belong in context for a file or symbol, and why." },
      { cmd: "/pulse", desc: "The files most likely to break next, formula on the card." },
      { cmd: "/handoff", desc: "The paste-ready shift report for the next session." },
    ],
  },
  {
    group: "The trail",
    items: [
      { cmd: "/why", desc: "Read the decision trail: why past changes were made." },
      { cmd: "/blame <file>", desc: "One decision per line for a single file." },
      { cmd: "/rewind", desc: "Restore an earlier turn and revert the edits made since." },
    ],
  },
  {
    group: "Spend and context",
    items: [
      { cmd: "/usage", desc: "Live provider quota and rate-limit snapshot." },
      { cmd: "/cost", desc: "Per-model token tally and estimated dollars, cache-aware." },
      { cmd: "/context", desc: "Context-window usage and where the tokens are going." },
      { cmd: "/compact <focus>", desc: "Compact the conversation, steering what the summary keeps." },
    ],
  },
  {
    group: "Session",
    items: [
      { cmd: "/model", desc: "Switch the active model mid-session." },
      { cmd: "/resume", desc: "Pick a previous session and continue it." },
      { cmd: "/plan", desc: "Enter read-only plan mode before acting." },
      { cmd: "/buddy", desc: "Hatch the pixel companion, or reset and hide it." },
    ],
  },
];

/** Composer prefixes available while typing. */
export const composerPrefixes: { prefix: string; desc: string }[] = [
  { prefix: "@path", desc: "Attach a file or directory listing with a gitignore-aware typeahead." },
  { prefix: "!command", desc: "Run a shell command immediately, no model turn. Output feeds the next message." },
  { prefix: "#note", desc: "Append a note to the project CLAUDE.md and re-apply memory to the live session." },
];

/** Sign-in routes. */
export const authMethods: { title: string; body: string }[] = [
  {
    title: "Subscription, OpenAI",
    body: "tomte login signs in with a ChatGPT Plus, Pro, Team, or Enterprise account over OAuth.",
  },
  {
    title: "Subscription, Anthropic",
    body: "Claude Pro or Max signs in over OAuth after you acknowledge the terms notice.",
  },
  {
    title: "API key",
    body: "Paste an OpenAI or Anthropic key, or let tomte pick up OPENAI_API_KEY and ANTHROPIC_API_KEY from the environment.",
  },
];

/** Install steps for the quickstart plate. */
export const quickstart: { step: string; cmd: string; note: string }[] = [
  { step: "Clone and install", cmd: "git clone https://github.com/ryan-mt/tomte && cd tomte\nmake install", note: "Builds in release and links tomte onto your PATH." },
  { step: "Sign in", cmd: "tomte login", note: "Opens a browser for OAuth, or prompts for an API key." },
  { step: "Run", cmd: "tomte", note: "Launches the terminal UI. Add resume to reopen a session." },
];

/** Prebuilt binary targets. */
export const binaries: { platform: string; archive: string }[] = [
  { platform: "Linux x86-64", archive: "tomte-x86_64-unknown-linux-gnu.tar.gz" },
  { platform: "macOS Intel", archive: "tomte-x86_64-apple-darwin.tar.gz" },
  { platform: "macOS Apple Silicon", archive: "tomte-aarch64-apple-darwin.tar.gz" },
  { platform: "Windows x86-64", archive: "tomte-x86_64-pc-windows-msvc.zip" },
];

/** Headless usage examples. */
export const headlessExamples: string[] = [
  'tomte chat "write a fibonacci function in Python"',
  'tomte chat --model gpt-5.5-pro --reasoning high "refactor module X"',
  "tomte prove --json   # gate CI on the project's own checks",
  "tomte handoff --out HANDOFF.md   # the shift report, scripted",
  "tomte run --cwd /srv/project --prompt-file nightly-task.md",
];

/** Security model. Honest about the Windows tradeoff. */
export const security: { title: string; body: string }[] = [
  {
    title: "Commands run in an OS sandbox",
    body: "run_shell runs inside an OS-level sandbox: Landlock and seccomp on Linux, sandbox-exec on macOS, confining writes to the workspace with outbound network off by default. On Windows it is best-effort process-tree cleanup only, so review destructive prompts there. On top of that, tomte flags obvious destructive commands like rm -rf on home or system paths, curl piped to a shell, mkfs, and force-pushes, and refuses them until you explicitly override.",
  },
  {
    title: "Secrets stay out of the shell",
    body: "Environment variables that look like secrets, with names containing TOKEN, SECRET, KEY, OPENAI, AWS, or GITHUB, plus connection strings and vendor prefixes, are stripped from child processes so the model cannot read them back.",
  },
  {
    title: "Writes are guarded",
    body: "Stale-file guards refuse a write when a file changed since the model last read it. auto_approve_write is false by default, and sub-agents inherit the parent approval policy.",
  },
  {
    title: "Credentials are owner-only",
    body: "OAuth uses PKCE and refreshes automatically. Tokens are written with owner-only permissions on Unix and an owner-only ACL on Windows. Project permission allow-lists reject symlinked paths so an allow decision cannot be redirected.",
  },
];

/** Configuration summary. */
export const configFields: { key: string; desc: string }[] = [
  { key: "model", desc: "Default model, for example gpt-5.5 or claude-fable-5." },
  { key: "reasoning_effort", desc: "none, minimal, low, medium, high, xhigh, or max." },
  { key: "verbosity", desc: "low, medium, or high." },
  { key: "auto_approve_read", desc: "Auto-approve read-only tools." },
  { key: "auto_approve_write", desc: "Auto-approve write tools. False by default." },
  { key: "fallback_models", desc: "Ordered list used for transparent failover." },
];

export const faq: { q: string; a: string }[] = [
  {
    q: "What does tomte do that other coding agents do not?",
    a: "Four things ship together here and nowhere else: a proof capsule built from your project's own checks (the model cannot fabricate it), a decision trail that survives switching models mid-project, a verifiable map of the repository that answers which files belong in context and why, and an agent tournament judged deterministically on measured evidence. Pulse and Handoff compose those indexes into a risk card and a shift report.",
  },
  {
    q: "Will my Claude Code or Codex setup work?",
    a: "Yes. tomte keeps the muscle memory: a terminal UI, slash commands, plan mode, composer prefixes, and inherited AGENTS.md and CLAUDE.md memory. Your existing skills and sub-agents are discovered automatically.",
  },
  {
    q: "Do I need an API key?",
    a: "No. You can sign in with a ChatGPT or Claude subscription over OAuth. API keys also work and unlock the full model catalogue. Environment keys are picked up automatically.",
  },
  {
    q: "Which providers and models are supported?",
    a: "The OpenAI GPT-5 family, Anthropic's Claude Fable 5 and Claude 4 families, and any OpenAI-compatible endpoint including local Ollama and LM Studio. See the Models page for the current catalogue.",
  },
  {
    q: "Is my code sent anywhere, and is it sandboxed?",
    a: "Your prompts and the files the agent reads go to the provider you choose, the same as any coding assistant. run_shell runs inside an OS-level sandbox (Landlock and seccomp on Linux, sandbox-exec on macOS; default workspace-write with outbound network off), and tomte flags obvious destructive commands on top. On Windows the sandbox is best-effort process cleanup only, so review destructive commands there.",
  },
  {
    q: "What platforms run it?",
    a: "Prebuilt binaries cover Linux x86-64, macOS on Intel and Apple Silicon, and Windows x86-64. You can also build from source with stable Rust.",
  },
  {
    q: "Why is it called tomte?",
    a: "The tomte is the Nordic farm spirit who keeps the household in order overnight: meticulous, quiet, and intolerant of sloppy work. It also hatches a pixel companion in the corner of the terminal, because a night watch is better with company.",
  },
];
