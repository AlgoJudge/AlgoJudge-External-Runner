# The external Runner, as one static binary in an image with nothing else in it.
#
# **Smaller than the sandboxing Runner's, and deliberately so.** That one holds
# the container runtime's socket and starts sibling containers; this one starts
# nothing and runs nothing. It signs in to somebody else's judging system, hands
# a submission over, and asks what it decided — so it needs no socket, no
# cgroups, no scratch directory, and no privileges of any kind.
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

# The two directories this Runner writes to, created here so that a **named
# volume mounted over either one inherits this ownership**. Docker seeds an empty
# volume from the image path, and a directory that does not exist in the image
# gives a volume owned by root — which a `nonroot` process cannot write.
#
# **The cache half was missing until 2026-08-31, and it broke every job.** The
# identity directory was here from the beginning because losing it is visible at
# once: the Runner cannot generate its key and does not start. The cache is not
# written until a job has been claimed, so its absence failed later and looked
# like something else — `create_dir_all` under a root-owned `/var` returns
# `EACCES`, the fetch of the participant's own source fails, and the job is
# reported as an infrastructure failure with the archive never contacted.
RUN mkdir -p /state/lib /state/cache

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build \
    /src/target/x86_64-unknown-linux-musl/release/algojudge-external-runner \
    /usr/local/bin/algojudge-external-runner

COPY --from=build --chown=65532:65532 /state/lib   /var/lib/algojudge-external-runner
COPY --from=build --chown=65532:65532 /state/cache /var/cache/algojudge-external-runner

# The identity is the state worth keeping, and it is meant to be a volume: losing
# it costs a re-registration and an administrator's approval. The cache is not —
# losing it costs one download.
#
# **There is no *package* cache here, and this said there was no cache at all.**
# The narrower sentence is the true one: an external problem has no package, so
# nothing is ever downloaded for a problem. The participant's own source is, on
# every job, through the protocol crate's cache and with its checksum verified —
# and it needs somewhere to live. Both paths are stated here rather than left to
# agree with a constant in the binary, which is how the second one came to be
# absent from the image for a fortnight.
ENV AJ_Runner__KeyPath=/var/lib/algojudge-external-runner/identity.key \
    AJ_Cache__Path=/var/cache/algojudge-external-runner

# No port is published and none is listened on. This Runner dials out twice — to
# the Server and to the judging system — and accepts nothing.

USER nonroot
ENTRYPOINT ["/usr/local/bin/algojudge-external-runner"]
