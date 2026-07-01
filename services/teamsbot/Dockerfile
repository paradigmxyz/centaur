# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

# Workspace manifests first so dependency install is a cached layer. teamsbot
# depends on the @centaur/* workspace packages, so they must be present for
# pnpm to link the `workspace:*` ranges.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/teamsbot/package.json services/teamsbot/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter teamsbot --prod --frozen-lockfile

COPY services/teamsbot/ services/teamsbot/
WORKDIR /repo/services/teamsbot

EXPOSE 3100
ENV PORT=3100
USER bun
CMD ["bun", "src/server.ts"]
