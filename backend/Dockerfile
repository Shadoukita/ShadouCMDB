FROM node:22-alpine AS build
WORKDIR /app
COPY package.json package-lock.json ./
COPY backend/package.json backend/
COPY frontend/package.json frontend/
RUN npm ci --workspace backend --include-workspace-root
COPY backend backend
COPY sql sql
RUN npm run build -w backend && npm prune --omit=dev --workspace backend

FROM node:22-alpine
WORKDIR /app
ENV NODE_ENV=production
COPY --from=build /app/node_modules node_modules
COPY --from=build /app/backend/package.json backend/package.json
COPY --from=build /app/backend/dist backend/dist
COPY --from=build /app/sql sql
WORKDIR /app/backend
USER node
EXPOSE 3000
# Migrations are a separate, explicit step: `docker compose run --rm migrate`.
CMD ["node", "dist/server.js"]
