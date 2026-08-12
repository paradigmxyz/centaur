# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/feishubot/package.json services/feishubot/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter feishubot --prod --frozen-lockfile

COPY services/feishubot/ services/feishubot/
WORKDIR /repo/services/feishubot

EXPOSE 3005
ENV PORT=3005
CMD ["bun", "src/server.ts"]
