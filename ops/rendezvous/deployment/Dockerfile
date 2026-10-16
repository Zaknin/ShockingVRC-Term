FROM rust:1.96.0-bookworm AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --locked --release -p shocking_vrc_rendezvous

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/target/release/shocking-vrc-rendezvous /usr/local/bin/shocking-vrc-rendezvous
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/shocking-vrc-rendezvous"]
