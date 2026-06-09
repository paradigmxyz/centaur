# syntax=docker/dockerfile:1.7

FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install -g pnpm@10.28.1

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY services/slackbot/package.json services/slackbot/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter slackbot --prod --frozen-lockfile

COPY services/slackbot/ services/slackbot/
WORKDIR /repo/services/slackbot

EXPOSE 3001
ENV PORT=3001
CMD ["bun", "src/index.ts"]
