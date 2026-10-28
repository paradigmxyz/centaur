# Evals

Evals measure the quality of LLM-backed behavior that unit tests cannot pin
down. They make real model calls, so they are run on demand, not in CI.

Evals use [promptfoo](https://www.promptfoo.dev/). Each eval is a directory
with a `promptfooconfig.yaml`, a dataset, and usually a custom provider that
calls the production code path instead of a copy of its prompt. Providers can
be TypeScript, Python, or any executable, so the same harness works for
services in any language.

## Running

`evals/` is its own pnpm workspace, so promptfoo's dependencies stay out of
the service lockfile.

```bash
cd evals
pnpm install --frozen-lockfile
export OPENAI_API_KEY=<key>      # optional: OPENAI_BASE_URL
pnpm run eval:message-overrides
pnpm run view                    # browse results locally
```

Pass extra promptfoo flags after the script name. Useful ones:

- `--repeat 5` runs each case several times to expose nondeterminism.
- `--filter-pattern <regex>` runs matching case descriptions only.
- `--no-write` skips saving results to the local promptfoo database.

The scripts disable promptfoo telemetry, update checks, and sharing. Results
stay in `~/.promptfoo` unless you pass `-o <file>`.

A failing case exits with code 100. A provider error, such as a timeout or HTTP
failure, is reported as an error rather than a failed case.

## Evals

| Directory | Measures |
| --- | --- |
| `message-overrides/` | The Slackbot v2 classifier that turns a message into harness, model, provider, and reasoning overrides. Scores each field separately. |

## Adding an eval

1. Create `evals/<name>/promptfooconfig.yaml` and a dataset file.
2. Call the shipped code from a custom provider so prompt, schema, and
   post-processing changes are measured together.
3. Make the provider return an `error` when the production code silently
   falls back, so outages do not score as correct negatives.
4. Add an `eval:<name>` script to `package.json` and a row above.
