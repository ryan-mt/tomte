/**
 * content.ts: single source of truth for Tomte's website.
 *
 * STABILITY CONTRACT: everything that changes between releases (version,
 * model catalogue, tool belt, slash commands, links) lives HERE and nowhere
 * else. To update the site for a new release, edit this file only. Page
 * components read from these exports and never hard-code volatile values.
 *
 * Copy style: no em-dashes anywhere (regular hyphen only), concrete language,
 * English throughout.
 */

export const site = {
  name: "tomte",
  /** Wordmark as shown in the masthead. */
  wordmark: "tomte",
  /** Latin-binomial conceit for the field-guide framing. */
  binomial: "Tomte terminalis",
  headline: "A calm, multi-model coding agent for your terminal.",
  /** Hero subtext: <= 20 words, no em-dash. */
  subhead:
    "Rust-fast and multi-model. One open-source binary, quiet and surgical, at home in any repository.",
  /** Longer one-paragraph description for meta + intro plates. */
  description:
    "Tomte is a single-binary coding agent for your terminal. Point it at OpenAI or Anthropic, drop it into any repository, and it reads, writes, runs, searches, and reasons through real work with a full tool belt and a terminal UI that stays out of the way.",
  license: "MIT",
  language: "Rust",
  /**
   * Kept for completeness as the canonical version, but the UI prefers
   * linking to the latest release rather than printing a fixed number.
   */
  version: "0.0.2",
  repoUrl: "https://github.com/ryan-mt/tomte",
  releasesUrl: "https://github.com/ryan-mt/tomte/releases",
  latestReleaseUrl: "https://github.com/ryan-mt/tomte/releases/latest",
  contributingUrl: "https://github.com/ryan-mt/tomte/blob/main/CONTRIBUTING.md",
  licenseUrl: "https://github.com/ryan-mt/tomte/blob/main/LICENSE",
} as const;

/** Field-guide "specimen card" facts. Creative framing over real attributes. */
export const specimen: { label: string; value: string }[] = [
  { label: "Genus / species", value: "Tomte terminalis" },
  { label: "Class", value: "Coding agent" },
  { label: "Order", value: "Terminal-dwelling" },
  { label: "Habitat", value: "Your repository" },
  { label: "Range", value: "OpenAI, Anthropic, OpenAI-compatible endpoints" },
  { label: "Build", value: "Rust, single binary, no daemon" },
  { label: "Diet", value: "Tokens" },
  { label: "Status", value: "Open source (MIT)" },
];

export type NavItem = { label: string; href: string };

export const nav: NavItem[] = [
  { label: "Overview", href: "/" },
  { label: "Field guide", href: "/field-guide" },
  { label: "Models", href: "/models" },
  { label: "Install", href: "/install" },
];

/** Headline capabilities. Each is grounded in a real, shipped feature. */
export type Capability = {
  /** short specimen-style code, e.g. for figure labels */
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
    body: "Twenty-five tools across files, shell, search, web, notebooks, sub-agents, todos, and plan mode. Streamed, schema-validated, and run in parallel where it is safe.",
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
 * Pillar 2, the decision trail ("memory of why"). Powers the moat demo on the
 * home page: every change is recorded with its reasoning and rejected
 * alternatives, and the record survives a mid-task switch to another model.
 */
export type Decision = {
  /** file:line the decision lives at */
  loc: string;
  /** the choice that was made */
  decision: string;
  /** the reasoning behind it */
  why: string;
  /** alternatives considered and dropped */
  rejected: string[];
  /** the model that recorded it */
  model: string;
  /** the turn it was decided on */
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

/** Models offered in the moat demo's "model in play" toggle. */
export const trailModels: { id: string; accent: "oai" | "ant" }[] = [
  { id: "gpt-5.5", accent: "oai" },
  { id: "claude-opus-4-8", accent: "ant" },
];

/** The tool belt, grouped exactly as the agent exposes it. 25 tools total. */
export type ToolGroup = { group: string; blurb: string; tools: string[] };

export const toolBelt: ToolGroup[] = [
  {
    group: "Files",
    blurb: "Read and edit with stale-file guards that refuse a write when a file changed since it was last read.",
    tools: ["read_file", "write_file", "edit_file", "multi_edit", "list_dir"],
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
    blurb: "Dispatch sub-agents, ask the user, invoke skills, and disclose tools progressively.",
    tools: ["dispatch_agent", "ask_user_question", "skill", "tool_search"],
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
    tag: "Claude 4 family",
    signIn: "Claude subscription (OAuth) or API key",
    body: "The Claude 4 family over the Messages API, with adaptive extended thinking on the newest models. A Claude Pro or Max subscription signs in over OAuth after a terms acknowledgement.",
  },
  {
    key: "compatible",
    name: "OpenAI-compatible",
    accent: "compat",
    tag: "Any endpoint",
    signIn: "Custom base URL and key in config.json",
    body: "Any OpenAI-compatible endpoint, hosted or local. Declare a base URL, key, and context limit under providers in config.json and address it as provider/model.",
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
  { id: "claude-opus-4-8", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-opus-4-7", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-opus-4-6", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-opus-4-5", provider: "Anthropic", context: "200K", note: "Prior Opus generation." },
  { id: "claude-sonnet-4-6", provider: "Anthropic", context: "1M", note: "Adaptive extended thinking." },
  { id: "claude-sonnet-4-5", provider: "Anthropic", context: "200K", note: "Prior Sonnet generation." },
  { id: "claude-haiku-4-5", provider: "Anthropic", context: "200K", note: "Fastest, lowest cost." },
];

/** Reasoning effort levels the agent understands. */
export const reasoningLevels = [
  "low",
  "medium",
  "high",
  "xhigh",
] as const;

/** Curated slash commands worth knowing. Grouped for a card-per-group layout. */
export type CommandGroup = { group: string; items: { cmd: string; desc: string }[] };

export const slashCommands: CommandGroup[] = [
  {
    group: "Spend and context",
    items: [
      { cmd: "/usage", desc: "Live provider quota and rate-limit snapshot." },
      { cmd: "/cost", desc: "Per-model token tally and estimated dollars, cache-aware." },
      { cmd: "/context", desc: "Context-window usage and where the tokens are going." },
    ],
  },
  {
    group: "Source control",
    items: [
      { cmd: "/diff", desc: "Show the current git diff." },
      { cmd: "/commit", desc: "Stage and commit with a Conventional-Commit message." },
      { cmd: "/commit-push-pr", desc: "Commit, push the branch, and open a PR with gh." },
    ],
  },
  {
    group: "Session",
    items: [
      { cmd: "/model", desc: "Switch the active model mid-session." },
      { cmd: "/resume", desc: "Pick a previous session and continue it." },
      { cmd: "/plan", desc: "Enter read-only plan mode before acting." },
    ],
  },
  {
    group: "Workspace",
    items: [
      { cmd: "/worktree", desc: "Create or exit an isolated git worktree." },
      { cmd: "/init", desc: "Create a CLAUDE.md for the project." },
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
  'echo "read CLAUDE.md and summarize" | tomte chat',
  "tomte run --cwd /srv/project --prompt-file nightly-task.md",
];

/** Security model. Honest about the no-sandbox tradeoff. */
export const security: { title: string; body: string }[] = [
  {
    title: "Destructive commands are flagged",
    body: "run_shell executes directly on your machine. There is no sandbox yet, so tomte flags obvious destructive commands like rm -rf on home or system paths, curl piped to a shell, mkfs, and force-pushes, and refuses them until you explicitly override.",
  },
  {
    title: "Secrets stay out of the shell",
    body: "Environment variables that look like secrets, with names containing TOKEN, SECRET, KEY, OPENAI, AWS, or GITHUB, are stripped from the child process so the model cannot read them back.",
  },
  {
    title: "Writes are guarded",
    body: "Stale-file guards refuse a write when a file changed since the model last read it. auto_approve_write is false by default, and sub-agents inherit the parent approval policy.",
  },
  {
    title: "Credentials are owner-only",
    body: "OAuth uses PKCE and refreshes automatically. Tokens are written with owner-only permissions on Unix. Project permission allow-lists reject symlinked paths so an allow decision cannot be redirected.",
  },
];

/** Configuration summary. */
export const configFields: { key: string; desc: string }[] = [
  { key: "model", desc: "Default model, for example gpt-5.5 or claude-opus-4-8." },
  { key: "reasoning_effort", desc: "low, medium, high, or xhigh." },
  { key: "verbosity", desc: "low, medium, or high." },
  { key: "auto_approve_read", desc: "Auto-approve read-only tools." },
  { key: "auto_approve_write", desc: "Auto-approve write tools. False by default." },
  { key: "fallback_models", desc: "Ordered list used for transparent failover." },
];

export const faq: { q: string; a: string }[] = [
  {
    q: "Will my Claude Code or Codex setup work?",
    a: "Yes. Tomte keeps the muscle memory you know: a terminal UI, slash commands, plan mode, composer prefixes, and inherited AGENTS.md and CLAUDE.md memory. It works with your existing setup rather than replacing it, and adds what they do not: one Rust binary, genuinely multi-model, and quiet by design.",
  },
  {
    q: "Do I need an API key?",
    a: "No. You can sign in with a ChatGPT or Claude subscription over OAuth. API keys also work and unlock the full model catalogue. Environment keys are picked up automatically.",
  },
  {
    q: "Which providers and models are supported?",
    a: "The OpenAI GPT-5 family, the Anthropic Claude 4 family, and any OpenAI-compatible endpoint you configure, including local ones. See the Models page for the current catalogue.",
  },
  {
    q: "Is my code sent anywhere, and is it sandboxed?",
    a: "Your prompts and the files the agent reads go to the provider you choose, the same as any coding assistant. run_shell runs directly on your machine with no sandbox yet, so review destructive commands. tomte flags the obvious ones.",
  },
  {
    q: "What platforms run it?",
    a: "Prebuilt binaries cover Linux x86-64, macOS on Intel and Apple Silicon, and Windows x86-64. You can also build from source with stable Rust.",
  },
  {
    q: "What is the pixel companion?",
    a: "A small pixel-art creature that hatches from an egg and sits in the corner of the terminal UI. Its species is rarity-weighted and seeded from your account, so it is stable for you and only re-rolls when you switch accounts.",
  },
];
