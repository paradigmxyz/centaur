/**
 * Inline message directives, cloned from slackbotv2 (which restored them from
 * the v1 slackbot):
 *   --claude | --claude-code | --amp | --codex   pick the harness for the thread
 *   --meta                                       codex via Meta AI direct
 *   --model <name> (or --model=<name>)           pick the model within that harness
 *   --fable | --opus | --sonnet | --haiku        model shortcuts (imply claude-code)
 *
 * Flags are stripped from the text before it reaches the agent. The harness
 * applies at session creation (the API pins a thread to one harness); the model
 * applies per turn via the blocks-protocol `model` field. `--model` accepts a
 * full model id (claude-sonnet-4-6, gpt-5.2, …) or a Claude alias
 * (fable/opus/sonnet/haiku) which expands to the full id.
 */

export type MessageOverrides = {
  cleanedText: string;
  harnessType?: string;
  model?: string;
  provider?: string;
};

// Flag name -> HarnessType wire value (serde lowercase of the Rust enum).
const HARNESS_FLAGS: Record<string, string> = {
  amp: "amp",
  claude: "claudecode",
  "claude-code": "claudecode",
  claudecode: "claudecode",
  codex: "codex",
};

const PROVIDER_FLAGS: Record<string, { provider: string; harnessType: string }> = {
  meta: { provider: "responses", harnessType: "codex" },
};

// Claude model aliases, usable both as bare flags (--opus) and as --model
// values (--model opus). Bare-flag form also implies the claude-code harness.
const CLAUDE_MODEL_ALIASES: Record<string, string> = {
  fable: "claude-fable-5",
  haiku: "claude-haiku-4-5",
  opus: "claude-opus-4-8",
  sonnet: "claude-sonnet-4-6",
};

const MODEL_SHORTCUTS: Record<string, { harnessType: string; model: string }> =
  Object.fromEntries(
    Object.entries(CLAUDE_MODEL_ALIASES).map(([alias, model]) => [
      alias,
      { harnessType: "claudecode", model },
    ]),
  );

// Values are one horizontal-whitespace-delimited token; a newline after the
// value starts the user's prompt, not part of the model value.
const MODEL_VALUE_SEPARATOR = String.raw`(?:[^\S\r\n]*=[^\S\r\n]*|[^\S\r\n]+)`;
const FLAG_VALUE_BOUNDARY = String.raw`(?=[^\S\r\n]|\r?\n|\r|<br\s*/?>|$)`;

const MODEL_FLAG_PATTERN = new RegExp(
  String.raw`(?:^|\s)--model${MODEL_VALUE_SEPARATOR}([A-Za-z0-9._/-]+)${FLAG_VALUE_BOUNDARY}`,
  "i",
);

export function extractMessageOverrides(text: string): MessageOverrides {
  let cleaned = text;
  let harnessType: string | undefined;
  let model: string | undefined;
  let provider: string | undefined;

  const modelMatch = MODEL_FLAG_PATTERN.exec(cleaned);
  if (modelMatch) {
    const value = modelMatch[1]!;
    model = CLAUDE_MODEL_ALIASES[value.toLowerCase()] ?? value;
    cleaned = stripMatch(cleaned, modelMatch);
  }

  for (const [flag, harness] of Object.entries(HARNESS_FLAGS)) {
    const match = flagPattern(flag).exec(cleaned);
    if (!match) continue;
    harnessType = harness;
    cleaned = stripMatch(cleaned, match);
  }

  for (const [flag, shortcut] of Object.entries(MODEL_SHORTCUTS)) {
    const match = flagPattern(flag).exec(cleaned);
    if (!match) continue;
    model ??= shortcut.model;
    harnessType ??= shortcut.harnessType;
    cleaned = stripMatch(cleaned, match);
  }

  for (const [flag, mapping] of Object.entries(PROVIDER_FLAGS)) {
    const match = flagPattern(flag).exec(cleaned);
    if (!match) continue;
    provider ??= mapping.provider;
    harnessType ??= mapping.harnessType;
    cleaned = stripMatch(cleaned, match);
  }

  return {
    cleanedText: cleaned === text ? text : cleaned.trim(),
    harnessType,
    model,
    provider,
  };
}

function flagPattern(flag: string): RegExp {
  return new RegExp(
    `(?:^|\\s)--${flag.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?=\\s|$)`,
    "i",
  );
}

function stripMatch(text: string, match: RegExpExecArray): string {
  const before = text.slice(0, match.index);
  const after = text
    .slice(match.index + match[0].length)
    .replace(/^(?:(?:\r\n?|\n)+|<br\s*\/?>)+/i, "");
  const separator =
    before && after && !/\s$/.test(before) && !/^\s/.test(after) ? " " : "";
  return `${before}${separator}${after}`;
}
