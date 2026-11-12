# syntax=docker/dockerfile:1
# MonitorFlow web interface: the static export (ADR-0014) served by Caddy,
# which also forwards /api to the server (ADR-0006). Build from the
# repository root:
#   docker build -f deploy/docker/web.Dockerfile -t monitorflow-web .

FROM node:26-trixie-slim AS build
ENV NEXT_TELEMETRY_DISABLED=1
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM caddy:2.11.8
# The user the compose file runs Caddy as (ADR-0110), which owns what Caddy
# and web-start.sh write: a new data volume starts with these owners.
RUN addgroup -S -g 10002 monitorflow-web \
    && adduser -S -D -H -u 10002 -G monitorflow-web -h /data monitorflow-web \
    && install -d -o 10002 -g 10002 /srv/monitorflow/run \
    && chown 10002:10002 /data /config
COPY deploy/caddy/Caddyfile deploy/caddy/trust.html /etc/caddy/
COPY --chmod=755 deploy/docker/web-start.sh /usr/local/bin/web-start
COPY --from=build /web/out /srv/monitorflow/web
ENTRYPOINT ["web-start"]
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
