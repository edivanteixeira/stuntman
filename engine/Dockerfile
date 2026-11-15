# Stuntman: one image with the server, the UI (Leptos/WASM) and the plugin catalog.
# docker build -t stuntman .   ·   docker compose up -d   (see docker-compose.yml)

# ---- build ----
# Runs on the build machine's own architecture and cross-compiles for the target one: no emulated rustc
# (rustc under QEMU crashes), so one `docker buildx build --platform linux/amd64,linux/arm64` works anywhere.
FROM --platform=$BUILDPLATFORM rust:1.96-trixie AS build
ARG TARGETARCH
ARG BUILDARCH
ARG TRUNK_VERSION=0.21.14
RUN rustup target add wasm32-unknown-unknown \
 && case "$(uname -m)" in aarch64) t=aarch64-unknown-linux-gnu ;; *) t=x86_64-unknown-linux-gnu ;; esac \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${t}.tar.gz" | tar -xz -C /usr/local/bin trunk
RUN case "$TARGETARCH" in amd64) t=x86_64-unknown-linux-gnu; g=x86-64; c=amd64 ;; arm64) t=aarch64-unknown-linux-gnu; g=aarch64; c=arm64 ;; esac \
 && echo "$t" > /rust-target && rustup target add "$t" \
 && if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends "gcc-${g}-linux-gnu" "libc6-dev-${c}-cross" && rm -rf /var/lib/apt/lists/*; \
    fi
WORKDIR /src
# Only what the build needs: editing a plugin or a test never recompiles the engine.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY web web
RUN cd web && trunk build --release
RUN t="$(cat /rust-target)" \
 && if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      cc="$(echo "$t" | sed -E 's/^(x86_64|aarch64)-unknown-linux-gnu$/\1-linux-gnu-gcc/')"; \
      T="$(echo "$t" | tr 'a-z-' 'A-Z_')"; export "CARGO_TARGET_${T}_LINKER=$cc" "CC_$(echo "$t" | tr '-' '_')=$cc"; \
    fi \
 && cargo build --release --bin stuntman --target "$t" \
 && mkdir -p /out && cp "target/$t/release/stuntman" /out/stuntman

# ---- run ----
FROM debian:trixie-slim
# git and unzip: Catalog › Import reads plugins from a git URL or a .zip
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git unzip tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data stuntman \
 && mkdir -p /data && chown stuntman /data
COPY --from=build /out/stuntman /usr/local/bin/stuntman
COPY --from=build /src/web/dist /app/web
COPY plugins /app/plugins
ENV STUNTMAN_DATA=/data \
    STUNTMAN_WEB=/app/web \
    STUNTMAN_PLUGINS=/app/plugins \
    STUNTMAN_PORT=8025 \
    STUNTMAN_FTP_PASSIVE_PORTS=30000-30009 \
    STUNTMAN_FTP_PASSIVE_HOST=127.0.0.1
USER stuntman
VOLUME /data
# UI/API/MCP · SMTP · webhooks · HTTP APIs (+TLS) · Key Vault · AMQP (+TLS) · Blob · S3 · OTLP grpc/http
# · LDAP · FTP (+passive range) · SFTP · IMAP · POP3 · Kafka · Schema Registry · RabbitMQ management · ActiveMQ (OpenWire) · GELF/StatsD (udp)
# · Service Bus management (5300, as the official emulator; also AMQP)
EXPOSE 8025 2525 8027 8028 8443 8444 5673 5671 5300 10000 9000 4317 4318 \
       3389 2121 30000-30009 2222 1143 1110 9092 8081 8085 4567 15672 61616 12201/udp 8125/udp
HEALTHCHECK --interval=30s --timeout=5s CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8025 && printf 'GET /api/env HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q 200 <&3"]
ENTRYPOINT ["/usr/bin/tini", "--", "stuntman"]
