# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

# Workspace manifests first so dependency install is a cached layer. googlechatbot
# depends on the @centaur/* workspace packages, so they must be present for
# pnpm to link the `workspace:*` ranges.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/googlechatbot/package.json services/googlechatbot/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter googlechatbot --prod --frozen-lockfile

COPY services/googlechatbot/ services/googlechatbot/
WORKDIR /repo/services/googlechatbot

EXPOSE 3101
ENV PORT=3101
USER bun
CMD ["bun", "src/server.ts"]
