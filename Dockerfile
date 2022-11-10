FROM rust:1.65.0@sha256:b0f2a9e48df82f009fda8ae777119e7983104a1b4dc47026653b6cdaf447d14b as builder

ENV TARGET=x86_64-unknown-linux-musl
RUN rustup target add ${TARGET}

RUN rm -f /etc/apt/apt.conf.d/docker-clean; echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

# borrowed (Ba Dum Tss!) from
# https://github.com/pablodeymo/rust-musl-builder/blob/7a7ea3e909b1ef00c177d9eeac32d8c9d7d6a08c/Dockerfile#L48-L49
RUN --mount=type=cache,target=/var/cache/apt --mount=type=cache,target=/var/lib/apt \
    apt-get update && \
    apt-get --no-install-recommends install -y \
    build-essential \
    musl-dev \
    musl-tools

# The following block
# creates an empty app, and we copy in Cargo.toml and Cargo.lock as they represent our dependencies
# This allows us to copy in the source in a different layer which in turn allows us to leverage Docker's layer caching
# That means that if our dependencies don't change rebuilding is much faster
WORKDIR /build
RUN cargo new rust-end-to-end-application
WORKDIR /build/rust-end-to-end-application
COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,target=/build/rust-end-to-end-application/target \
    cargo build --release --target ${TARGET}

# now we copy in the source which is more prone to changes and build it
COPY src ./src
# --release not needed, it is implied with install
RUN --mount=type=cache,target=/build/rust-end-to-end-application/target \
    cargo install --path . --target ${TARGET} --root /output

FROM alpine:3.16.2@sha256:d6be1101f945d8f3d9fdc94c0df90884ffad8d4b945968ceb9f9055722c208f0

RUN addgroup -S appgroup && adduser -S appuser -G appgroup
USER appuser

WORKDIR /app
COPY --from=builder /output/bin/rust-end-to-end-application /app
ENTRYPOINT ["/app/rust-end-to-end-application"]
