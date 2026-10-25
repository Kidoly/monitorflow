# syntax=docker/dockerfile:1
# MonitorFlow web interface: the static export (ADR-0014) served by Caddy,
# which also forwards /api to the server (ADR-0006). Build from the
# repository root:
#   docker build -f deploy/docker/web.Dockerfile -t monitorflow-web .

FROM node:22-trixie-slim AS build
ENV NEXT_TELEMETRY_DISABLED=1
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM caddy:2.11.7
COPY deploy/caddy/Caddyfile /etc/caddy/Caddyfile
COPY --from=build /web/out /srv/monitorflow/web
