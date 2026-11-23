FROM rust:1.99.1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release --locked -p weave-mcp

FROM debian:bookworm-slim

COPY --from=builder /src/target/release/weave-mcp /usr/local/bin/weave-mcp
ENTRYPOINT ["/usr/local/bin/weave-mcp"]
