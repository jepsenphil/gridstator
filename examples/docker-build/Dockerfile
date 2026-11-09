# syntax=docker/dockerfile:1
FROM node:24-bookworm-slim AS base
WORKDIR /app

FROM base AS build
ARG APP_VERSION=dev
COPY package.json ./
COPY scripts ./scripts
COPY src ./src
# A build secret is available only during this step and never stored in the image.
RUN --mount=type=secret,id=NPM_TOKEN \
    if [ -f /run/secrets/NPM_TOKEN ]; then echo "build secret present"; else echo "no build secret"; fi
RUN APP_VERSION=$APP_VERSION npm run build

FROM base AS test
COPY --from=build /app/dist ./dist
RUN test -f dist/server.js && grep -q "version" dist/server.js && echo "smoke test ok"

FROM base AS runner
ENV NODE_ENV=production
COPY --from=build /app/dist ./dist
COPY package.json ./
USER node
EXPOSE 3000
CMD ["node", "dist/server.js"]
