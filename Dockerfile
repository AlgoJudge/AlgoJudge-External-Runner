# The UVa Runner, as one static binary in an image with nothing else in it.
#
# **Smaller than the sandboxing Runner's, and deliberately so.** That one holds
# the container runtime's socket and starts sibling containers; this one starts
# nothing and runs nothing. It logs in to onlinejudge.org, posts a form, and
# polls uHunt for the verdict — so it needs no socket, no cgroups, no scratch
# directory, and no privileges of any kind.
#
# `musl` rather than glibc so the binary carries no dynamic loader, which is what
# lets the final stage be a distroless image with no shell and no package
# manager. `rustls` with bundled roots means no `ca-certificates` package and no
# OpenSSL, which is the other half of why the final image can be `static`.
#
# The base digest is the one `Dockerfile.toolchain` and AlgoJudge-Runner pin, so
# all three build against the same compiler.

FROM rust@sha256:3b2879047d42784ca9403ad20c51ed3df361a50f1df96f5777d39b4e33aa65cd AS build

# `musl-gcc` is needed by the one C dependency in the graph — `ring`, under
# rustls. Everything else is pure Rust.
RUN apt-get update \
    && apt-get install --no-install-recommends -y musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

WORKDIR /src

# Manifests first, so a change to the source does not re-resolve or re-download
# the dependency graph — which here includes cloning `aj-protocol` from git.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --target x86_64-unknown-linux-musl \
    && rm -r src

COPY src src
# The stubs' fingerprints would otherwise let cargo think the crate is current,
# and the image would quietly ship a binary built from an empty `main`.
RUN find src -name '*.rs' -exec touch {} + \
    && cargo build --release --target x86_64-unknown-linux-musl

# The one directory this Runner writes to, created here so that a **named volume
# mounted over it inherits this ownership**. Docker seeds an empty volume from
# the image path, and a directory that does not exist in the image gives a volume
# owned by root — which a `nonroot` process cannot write, and which fails at the
# first thing the Runner does, generating its key.
RUN mkdir -p /state/lib

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build \
    /src/target/x86_64-unknown-linux-musl/release/algojudge-runner-uva \
    /usr/local/bin/algojudge-runner-uva

COPY --from=build --chown=65532:65532 /state/lib /var/lib/algojudge-runner-uva

# The identity is the only state there is, and it is meant to be a volume: losing
# it costs a re-registration and an administrator's approval. There is no package
# cache here — this Runner downloads no packages, because it evaluates nothing.
ENV AJ_Runner__KeyPath=/var/lib/algojudge-runner-uva/identity.key

# No port is published and none is listened on. This Runner dials out twice — to
# the Server and to the archive — and accepts nothing.

USER nonroot
ENTRYPOINT ["/usr/local/bin/algojudge-runner-uva"]
