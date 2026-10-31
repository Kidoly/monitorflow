# A tiny signed apt and dnf upstream for deploy/smoke-test.sh (ADR-0067).
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends apt-utils createrepo-c dpkg-dev gnupg python3 rpm \
    && rm -rf /var/lib/apt/lists/*
COPY build.sh /build.sh
RUN sh /build.sh
CMD ["python3", "-m", "http.server", "8000", "--directory", "/srv"]
