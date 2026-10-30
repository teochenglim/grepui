# syntax=docker/dockerfile:1.7
#
# grepui image: one binary (UI embedded), non-root, no volume needed (state lives in GreptimeDB).
#   docker build -t grepui --build-arg VERSION=v0.1.0 .
#   docker run -p 8080:8080 -e GREPTIME_MYSQL=mysql://root@host.docker.internal:4002/public grepui

# ---- binary -----------------------------------------------------------------
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY web ./web
ARG VERSION=dev
ENV GREPUI_VERSION=${VERSION}
RUN cargo build --release --locked && cp target/release/grepui /grepui

# ---- runtime: distroless, uid 65532 -------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot
ARG VERSION=dev
LABEL org.opencontainers.image.title="grepui" \
      org.opencontainers.image.description="Multi-tenant observability UI for GreptimeDB" \
      org.opencontainers.image.source="https://github.com/teochenglim/grepui" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}"
COPY --from=build /grepui /usr/local/bin/grepui
ENV GREPUI_LISTEN=0.0.0.0:8080
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/grepui", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/grepui"]
CMD ["serve"]
