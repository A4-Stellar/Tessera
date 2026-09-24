# Production-grade, lightweight Dockerfile for Tessera Synthetic Monitor
FROM node:20-alpine AS runtime

WORKDIR /app

# Set non-root user for security compliance
USER node

# Copy application source and manifests
COPY --chown=node:node package.json ./
COPY --chown=node:node src ./src

ENV NODE_ENV=production \
    CHECK_INTERVAL_SECONDS=300 \
    CONSECUTIVE_FAILURE_THRESHOLD=2 \
    PROBE_TIMEOUT_MS=10000

# Container healthcheck running a single synthetic cycle verification
HEALTHCHECK --interval=60s --timeout=15s --start-period=10s --retries=3 \
  CMD node src/index.js --once || exit 1

ENTRYPOINT ["node", "src/index.js"]
