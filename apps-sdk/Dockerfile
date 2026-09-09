# syntax=docker/dockerfile:1.7

FROM rust:1.97-bookworm AS leio-code-builder

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates pkg-config \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /src/leio-code

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY vendor ./vendor
# Workspace member: cargo refuses to load the manifest without it, even though
# this image only builds the leio-code binary.
COPY crates ./crates

RUN cargo build --release --locked --bin leio-code

FROM node:22-bookworm-slim AS runtime

ENV NODE_ENV=production \
  LEIO_APPS_SDK_HOST=0.0.0.0 \
  LEIO_APPS_SDK_PORT=3333 \
  LEIO_CODE_BIN=/usr/local/bin/leio-code \
  LEIO_CODE_REPO_ROOT=/workspace/repo \
  LEIO_CODE_BAKED_REPO_ROOT=/workspace/baked-repo

WORKDIR /opt/leio-code

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates git \
  && rm -rf /var/lib/apt/lists/*

COPY --from=leio-code-builder /src/leio-code/target/release/leio-code /usr/local/bin/leio-code
COPY mcp ./mcp

WORKDIR /opt/leio-code/apps-sdk

COPY apps-sdk/package.json apps-sdk/package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force

COPY apps-sdk/auth.js ./auth.js
COPY apps-sdk/tenant-scope.mjs ./tenant-scope.mjs
COPY apps-sdk/repo-url.js ./repo-url.js
COPY apps-sdk/output-schemas.js ./output-schemas.js
COPY apps-sdk/tool-annotations.js ./tool-annotations.js
COPY apps-sdk/validate-repo-url.mjs ./validate-repo-url.mjs
COPY apps-sdk/server.js ./server.js
COPY apps-sdk/auth-summary.mjs ./auth-summary.mjs
COPY apps-sdk/entrypoint.sh ./entrypoint.sh
COPY apps-sdk/keycloak ./keycloak
COPY apps-sdk/public ./public
COPY . /workspace/baked-repo

RUN chmod +x ./entrypoint.sh

EXPOSE 3333

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:3333/health').then((res) => { if (!res.ok) process.exit(1); }).catch(() => process.exit(1));"

CMD ["./entrypoint.sh"]
