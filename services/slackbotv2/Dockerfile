# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

# Workspace manifests first so dependency install is a cached layer. slackbotv2
# depends on the @centaur/* workspace packages, so they must be present for
# pnpm to link the `workspace:*` ranges.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/slackbotv2/package.json services/slackbotv2/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter slackbotv2 --prod --frozen-lockfile

COPY services/slackbotv2/ services/slackbotv2/
# Harness config files: console-session-link.ts imports the baked default
# models (harness/claude/settings.json, harness/codex/config.toml) so the
# Slack Console-link line can name the model a harness runs without a
# hardcoded copy of those values.
COPY harness/ harness/
WORKDIR /repo/services/slackbotv2

EXPOSE 3001
ENV PORT=3001
CMD ["bun", "src/server.ts"]
