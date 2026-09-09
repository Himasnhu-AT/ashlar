# Builds the Tracer server into a small image. Build from the repository root:
#   docker build -f deploy/Dockerfile -t tracer-server .
FROM rust:1-slim-bookworm AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --locked -p tracer-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data tracer && mkdir -p /data && chown tracer /data
COPY --from=build /src/target/release/tracer-server /usr/local/bin/tracer-server
USER tracer
ENV TRACER_BIND=0.0.0.0:8090 TRACER_DATA_DIR=/data
VOLUME /data
EXPOSE 8090
ENTRYPOINT ["tracer-server"]
CMD ["serve"]
