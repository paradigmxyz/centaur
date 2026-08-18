# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

# Workspace manifests first so dependency install is a cached layer. linearbot depends on the
# @centaur/* workspace packages, so they must be present for pnpm to link the `workspace:*` ranges.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/linearbot/package.json services/linearbot/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter linearbot --prod --frozen-lockfile

COPY services/linearbot/ services/linearbot/
WORKDIR /repo/services/linearbot

EXPOSE 3001
ENV PORT=3001
# Run as the non-root `bun` user (uid/gid 1000) shipped by the base image; the bot only needs
# read access to /repo and listens on 3001 (>1024), so no ownership changes are required.
USER bun
CMD ["bun", "src/server.ts"]
