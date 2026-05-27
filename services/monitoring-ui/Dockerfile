FROM oven/bun:1.3.13-slim AS build

WORKDIR /app

COPY services/monitoring-ui/package.json ./
RUN bun install

COPY services/monitoring-ui/ ./
RUN bun run build

FROM oven/bun:1.3.13-slim

WORKDIR /app
ENV NODE_ENV=production

COPY --from=build /app/dist ./dist
COPY --from=build /app/server ./server

EXPOSE 3002
ENV PORT=3002
CMD ["bun", "server/index.ts"]
