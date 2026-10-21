---
name: prompt-iteration
description: "Try edited Centaur persona, prompt overlay, or base system prompt changes live from inside a sandbox by running a fresh child agent on the recomposed prompt. Use when iterating on tools/personas/*/PROMPT.md, services/sandbox/SYSTEM_PROMPT.md, or an overlay repo's prompt without redeploying."
---

# Prompt Iteration

The running session's prompt cannot change: personas are loaded when api-rs
starts and pinned when a session is created, and `compose-system-prompt` builds
`AGENTS.md` once at sandbox startup. To test an edit, recompose the prompt into
a scratch directory and run a fresh, one-shot harness process there.

The child is a local process, not a durable Centaur session. It reaches the
model through this sandbox's normal proxy path. Nothing is deployed.

## Workflow

1. Edit the prompt in a writable checkout (`git-branch <org/repo> <slug>`, then
   edit under `~/branches/<org>/<repo>`):
   - persona: `tools/personas/<id>/PROMPT.md`
   - base or overlay: `services/sandbox/SYSTEM_PROMPT.md`

2. Compose the prompt. By default this uses the session's current base,
   overlays, and persona:

   ```bash
   S=.agents/skills/prompt-iteration/scripts/compose_preview.sh
   # Persona edit:
   $S --persona ~/branches/<org>/<repo>/tools/personas/<id>/PROMPT.md /tmp/prompt-preview
   # Overlay-repo edit (replaces ~/github/<org>/<repo> as an overlay):
   $S --repo ~/branches/<org>/<repo> /tmp/prompt-preview
   # Base prompt edit (Centaur's own services/sandbox/SYSTEM_PROMPT.md):
   $S --base ~/branches/<org>/<repo>/services/sandbox/SYSTEM_PROMPT.md /tmp/prompt-preview
   ```

   Flags combine. Use `--no-persona` to compose without a persona. Read
   `/tmp/prompt-preview/AGENTS.md` to confirm the sections and their order
   before you run anything. The preview directory also links this session's
   installed skills, so the child sees the same skills.

3. Run a fresh child on the composed prompt, from the preview directory so it
   reads that `AGENTS.md`. Use the harness the persona targets. If the persona
   pins a model in `[tool.centaur.models]`, pass that model.

   ```bash
   cd /tmp/prompt-preview
   claude -p --append-system-prompt-file AGENTS.md "<probe message>"
   codex exec --skip-git-repo-check --ephemeral "<probe message>"
   ```

   These flags match how `harness-server` passes the prompt to each harness.
   Add `--model <model>` to either command if you need a specific model.

4. Compare runs. Send the same probe messages before and after the edit (or with
   `--no-persona`), and judge the replies on the behavior the edit is meant to
   change. Repeat steps 1–3 until it's right. Then commit the edit and open a
   PR as usual.

## Limits

The child gets the same composed prompt, skills, harness config, tools, and
proxy credentials as this session. It differs in these ways:

- It runs on the harness config's default model and effort. Real turns use the
  model api-rs picks: the persona's pin, the channel default, or the user's
  flags. Pass `--model` to match.
- It gets one bare message: no thread history, and none of the context the
  chat ingress adds (sender, channel, attachments). If the prompt reacts to that
  context, write it into the probe message.
- It bypasses `harness-server`, so Nanocodex subagent tools and the other
  harnesses (Pi, Amp, Hermes) are not covered.
- It does not show persona selection, persona model pinning, or Slack
  rendering. Those require a deploy, and persona registry edits also need an
  api-rs restart.
- Keep probe messages free of secrets. The child runs with this sandbox's
  credentials, so give it the same tasks you would run here.
