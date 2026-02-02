# Noctum Docker Image
# Multi-stage build for sandboxed simulation testing

FROM node:20-alpine AS builder

WORKDIR /app

# Copy package files for dependency installation
COPY package.json package-lock.json ./
COPY packages/cli/package.json ./packages/cli/
COPY packages/web/package.json ./packages/web/

# Install all dependencies during build phase
RUN npm ci

# Copy source code
COPY packages/ ./packages/

# Build all packages
RUN npm run build

# Production stage
FROM node:20-alpine AS production

WORKDIR /app

# Copy package files
COPY package.json package-lock.json ./
COPY packages/cli/package.json ./packages/cli/
COPY packages/web/package.json ./packages/web/

# Install production dependencies only
RUN npm ci --omit=dev

# Copy built artifacts from builder
COPY --from=builder /app/packages/cli/dist ./packages/cli/dist
COPY --from=builder /app/packages/web/dist ./packages/web/dist

# Create a directory for the target project (mounted at runtime)
RUN mkdir -p /project

# Expose port for the web dashboard
EXPOSE 3000

# Set working directory to project mount point
WORKDIR /project

# Start the Noctum server
# --dir /project: serve the mounted project directory
# --port 3000: fixed port for container
# --host 0.0.0.0: allow connections from outside container
CMD ["node", "/app/packages/cli/dist/index.js", "serve", "--port", "3000", "--host", "0.0.0.0", "--dir", "/project"]
