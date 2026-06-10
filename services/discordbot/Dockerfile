FROM oven/bun:1.3.13-slim

WORKDIR /repo
ENV NODE_ENV=production

RUN bun install -g pnpm@10.28.1

# Workspace manifests first so dependency install is a cached layer. discordbot depends on the
# @centaur/* workspace packages, so they must be present for pnpm to link the `workspace:*` ranges.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY packages/ packages/
COPY services/discordbot/package.json services/discordbot/package.json
RUN pnpm install --filter discordbot --prod --frozen-lockfile

COPY services/discordbot/ services/discordbot/
WORKDIR /repo/services/discordbot

EXPOSE 3001
ENV PORT=3001
CMD ["bun", "src/server.ts"]
