import type { Ctx } from "../centaur";

// Prose linting through the OpenAI Decisions API: four score questions (wordiness,
// mannered prose, hedging, citations) are sent with the draft as evidence, and the
// per-dimension level probabilities are normalized to a 0-1 score.

const DEFAULT_BASE = "https://api.openai.com/v1";
const MODEL = "gpt-6-luna";

interface Dimension {
  name: string;
  instructions: string;
  levels: [string, string][];
}

/** Levels run from clean (index 0) to worst (last index); scores are normalized to 0-1. */
const DIMENSIONS: Dimension[] = [
  {
    name: "wordiness",
    instructions:
      "Does the response use more words than its content needs? Count as " +
      "padding: preamble, restating the question, methodology or checks " +
      "nobody asked for, secondary examples, extra sources, closing lines " +
      "that repeat the opening, and wordy phrases such as 'in order to' or " +
      "'there are a number of'. Judge length against what was asked.",
    levels: [
      ["Tight", "Every sentence is an answer, reason, number, or check. Nothing to cut."],
      ["Mostly tight", "A few wordy phrases could be shorter, but no sentence is filler."],
      [
        "Padded",
        "Has filler sentences; could lose about a quarter of its words without losing meaning.",
      ],
      ["Bloated", "The answer is buried; it could be said in under half the words."],
    ],
  },
  {
    name: "mannered_prose",
    instructions:
      "Does the response use mannered, AI-sounding prose instead of plain " +
      "words? Tics include: 'not X, it's Y' or 'isn't just X, it's Y' " +
      "contrasts; rhetorical questions it answers itself; triplets for " +
      "rhythm; dramatic colon or em-dash reveals; stock openers and closers " +
      "('Great question', 'Here's the thing', 'Let's dive in', 'The real " +
      "question is', 'In short'); grand framing ('In today's rapidly " +
      "evolving landscape'); and inflated words (delve, crucial, robust, " +
      "landscape, tapestry, seamless, game-changer, unlock).",
    levels: [
      ["Plain", "Reads like a sharp colleague messaging a partner. No tics."],
      ["One tic", "One mannered construction or inflated word in otherwise plain prose."],
      ["Several tics", "Several tics; the prose noticeably sounds AI-written."],
      ["Pervasive", "Tics drive the structure; most sentences are mannered."],
    ],
  },
  {
    name: "hedging",
    instructions:
      "Does the response hedge instead of committing to a position? Count " +
      "as hedging: caveats that would not change the decision, repeated " +
      "caveats, stacked qualifiers ('may', 'could potentially'), wide " +
      "ranges instead of a base case, labels like 'illustrative' or " +
      "'provisional', and phrases such as 'doesn't establish', 'my inference is', 'not an " +
      "underwritten', or 'starting hypothesis'. One caveat that would " +
      "change the decision is not hedging.",
    levels: [
      ["Committed", "Takes a clear position with its main reason. No needless caveats."],
      ["Measured", "Takes a position; adds a minor caveat or qualifier it could drop."],
      ["Hedged", "The position is diluted by several caveats or qualifiers."],
      ["Evasive", "Avoids a position; mostly caveats, ranges, and disclaimers."],
    ],
  },
  {
    name: "citations",
    instructions:
      "How many sources does the response cite or discuss? Count each " +
      "distinct source named, linked, or attributed (such as 'IDC 2025' or " +
      "'per Bloomberg'). Lists of sources consulted, narrating the research " +
      "('pieced together from', 'based on my research across'), and notes " +
      "about sources, files, or channels it could not access all count as " +
      "discussing sources. Links to the response's own attached files or " +
      "artifacts are not sources.",
    levels: [
      ["None", "Cites or discusses no sources."],
      ["One source", "Cites a single source."],
      ["A few sources", "Cites two or three sources, or notes a source it could not access."],
      [
        "Many sources",
        "Cites four or more sources, lists sources consulted, or narrates its research.",
      ],
    ],
  },
];

/** One dimension's verdict; `refused: true` when the model declined to score it. */
export interface DimensionScore {
  /** 0 (clean) to 1 (worst); present unless `refused`. */
  score?: number;
  /** Label of the most probable level, such as `Tight` or `Bloated`. */
  level?: string | null;
  /** The model's confidence in that level, 0-1. */
  confidence?: number;
  /** True when the Decisions API returned no score for this dimension. */
  refused?: boolean;
}

/** A lint result: `score` is the worst dimension score, `dimensions` the breakdown. */
export interface ProseLintResult {
  /** Highest dimension score, 0 (clean) to 1 (worst). */
  score: number;
  /** One entry per dimension, keyed by `wordiness`, `mannered_prose`, `hedging`, `citations`. */
  dimensions: Record<string, DimensionScore>;
}

function base(ctx: Ctx): string {
  return (ctx.env.OPENAI_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx): Record<string, string> {
  const value: Record<string, string> = { "content-type": "application/json" };
  // Behind iron-proxy `Authorization` is replaced on the wire; locally the key comes from the env.
  if (ctx.env.OPENAI_API_KEY) value["Authorization"] = `Bearer ${ctx.env.OPENAI_API_KEY}`;
  return value;
}

/** OpenAI errors are `{ error: { message } }`; fall back to the raw body. */
async function decide(ctx: Ctx, payload: unknown): Promise<any> {
  const res = await ctx.fetch(`${base(ctx)}/decisions`, {
    method: "POST",
    headers: headers(ctx),
    body: JSON.stringify(payload),
  });
  const text = await res.text();
  let body: any;
  try {
    body = text ? JSON.parse(text) : {};
  } catch {
    body = undefined;
  }
  if (!res.ok) {
    const error = body?.error;
    const detail = error === undefined ? text : typeof error === "string" ? error : error.message ?? JSON.stringify(error);
    throw new Error(`OpenAI Decisions API returned HTTP ${res.status}: ${detail}`);
  }
  if (body === undefined) throw new Error(`OpenAI Decisions API returned non-JSON: ${text.slice(0, 500)}`);
  return body;
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}

/**
 * Score a draft from 0 (clean) to 1 (worst) on four dimensions: `wordiness`, `mannered_prose`,
 * `hedging` and `citations`. `text` is the exact draft; `context` is the request the draft
 * answers, which improves the wordiness and hedging judgments. Returns the worst dimension
 * score as `score` plus each dimension's `score`, most probable `level` (for example `Padded`)
 * and `confidence`; a dimension the model refuses comes back as `{ refused: true }`. Callers
 * choose their own rewrite threshold. Throws when every dimension was refused.
 */
export async function score(
  args: { text: string; context?: string },
  ctx: Ctx,
): Promise<ProseLintResult> {
  if (!args.text || !args.text.trim()) throw new Error("text must not be empty");
  let evidence = `Response to score:\n${args.text}`;
  if (args.context && args.context.trim()) {
    evidence = `Request:\n${args.context.trim()}\n\n${evidence}`;
  }

  const answers: any[] =
    (
      await decide(ctx, {
        model: MODEL,
        input: evidence,
        questions: DIMENSIONS.map((dimension) => ({
          type: "score",
          name: dimension.name,
          instructions: dimension.instructions,
          levels: dimension.levels.map(([label, description]) => ({ label, description })),
        })),
      })
    ).answers ?? [];

  const by_name: Record<string, any> = {};
  for (const answer of answers) {
    if (answer?.name) by_name[answer.name] = answer;
  }

  const dimensions: Record<string, DimensionScore> = {};
  for (const dimension of DIMENSIONS) {
    const answer = by_name[dimension.name] ?? {};
    if (answer.type !== "score" || typeof answer.score !== "number") {
      dimensions[dimension.name] = { refused: true };
      continue;
    }
    const probabilities: any[] = answer.probabilities ?? [];
    let top: any = undefined;
    for (const probability of probabilities) {
      if (top === undefined || (probability?.probability ?? 0) > (top?.probability ?? 0)) top = probability;
    }
    dimensions[dimension.name] = {
      score: round(answer.score / (dimension.levels.length - 1)),
      level: top?.label ?? null,
      confidence: answer.confidence,
    };
  }

  const scores = Object.values(dimensions)
    .map((dimension) => dimension.score)
    .filter((value): value is number => typeof value === "number");
  if (!scores.length) throw new Error("Decisions API refused to score every dimension");
  return { score: Math.max(...scores), dimensions };
}
