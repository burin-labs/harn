# syntax=docker/dockerfile:1.7

FROM --platform=$BUILDPLATFORM rust:1.95 AS builder

ARG TARGETARCH

WORKDIR /work

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_TARGET_DIR=/tmp/harn-target

RUN case "${TARGETARCH}" in \
        amd64) \
            echo "x86_64-unknown-linux-gnu" > /tmp/rust-target \
            ;; \
        arm64) \
            apt-get update \
            && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
            && rm -rf /var/lib/apt/lists/* \
            && echo "aarch64-unknown-linux-gnu" > /tmp/rust-target \
            ;; \
        *) \
            echo "unsupported TARGETARCH: ${TARGETARCH}" >&2 \
            && exit 1 \
            ;; \
    esac \
    && rustup target add "$(cat /tmp/rust-target)"

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/tmp/harn-target \
    RUST_TARGET="$(cat /tmp/rust-target)" \
    && if [ "${TARGETARCH}" = "arm64" ]; then export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc; fi \
    && cargo build --locked --release -p harn-cli --bin harn --bin harn-container-probe --target "${RUST_TARGET}" \
    && install -d /out/usr/local/bin /out/etc/harn /out/var/lib/harn/state \
    && install -m 0755 "/tmp/harn-target/${RUST_TARGET}/release/harn" /out/usr/local/bin/harn \
    && install -m 0755 "/tmp/harn-target/${RUST_TARGET}/release/harn-container-probe" /out/usr/local/bin/harn-container-probe \
    && printf "# Mount a Harn orchestrator manifest here.\n" > /out/etc/harn/triggers.toml

FROM gcr.io/distroless/cc-debian12

WORKDIR /var/lib/harn

# Inject listener auth and provider secrets at runtime via
# HARN_ORCHESTRATOR_API_KEYS, HARN_PROVIDER_*, HARN_SECRET_*,
# OPENAI_API_KEY, ANTHROPIC_API_KEY, or similar provider-specific env vars.
ENV HARN_SECRET_PROVIDERS=env \
    HARN_ORCHESTRATOR_API_KEYS= \
    HARN_ORCHESTRATOR_MANIFEST=/etc/harn/triggers.toml \
    HARN_ORCHESTRATOR_LISTEN=0.0.0.0:8080 \
    HARN_ORCHESTRATOR_STATE_DIR=/var/lib/harn/state \
    RUST_LOG=info

COPY --from=builder --chown=10001:10001 /out/ /

EXPOSE 8080

USER 10001:10001

ENTRYPOINT ["/usr/local/bin/harn", "orchestrator", "serve"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["/usr/local/bin/harn-container-probe"]
