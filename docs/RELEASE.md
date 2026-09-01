# Releasing the external Runner

For whoever cuts the release. Somebody installing the product wants
[AlgoJudge-Ops](https://github.com/AlgoJudge/AlgoJudge-Ops) instead, where this
is the `external-runner` profile.

## Where the version lives

**`Cargo.toml`, `[package] version`, one line**, and `Cargo.lock` beside it.
This is a single crate rather than a workspace, so there is nothing else to
keep in step.

**It is released on its own schedule.** The Server, the Client and the
sandboxing Runner are one set that a stack pulls by one tag; this is a fourth
image with its own repository, and an installation that does not forward
anywhere never pulls it at all.

## What a tag does

`.github/workflows/release.yml` runs on a pushed tag matching `v*`, refuses one
that does not point at a commit on `main`, and publishes one image —
`ghcr.io/algojudge/algojudge-external-runner` — under `0.1.0`, `0.1`, `0` and
`latest`. A prerelease publishes its own tag alone.

Before pushing it checks two things about the image itself: that it **refuses to
start with nothing configured**, and that its two directories exist and belong to
`nonroot`. Both are properties an operator depends on and neither is visible from
the source alone.

## Before the tag

- [ ] `Cargo.toml` says the version being released, and `Cargo.lock` agrees.
- [ ] `README.md` names that version under *The published image*.
- [ ] The commit is on `main`, and **its** CI run is green.
- [ ] `./x gate` — fmt, clippy with warnings as errors, a release build and the
      whole suite. Rust is not a prerequisite; `x` runs cargo in a container
      pinned by digest.
- [ ] That suite includes the test comparing `.env.example` against what the
      code reads, both ways. A green run is parity of the **key set** and not a
      proofread: read the comments and the sample values too.
- [ ] The `Dockerfile` base digest is the one intended, and **the same digest
      `AlgoJudge-Runner` pins**. Two Rust images a month apart is two compilers
      nobody chose.
- [ ] Run it end to end at least once against a Server: the development compose,
      then `AJ_TEST_SERVER=… ./x test -- --include-ignored`.
- [ ] **The credential in your own `.env` is not in the commit.** `.env.example`
      gives no value to either secret, and nothing else should.

## After the tag

Nothing here depends on this image, and it depends on nothing here: an
installation adds the `external-runner` profile when it wants one, and
`AlgoJudge-Ops` pulls it by the moving major `0`.

Two things outside this repository decide whether it is ever handed work —
external judging being on for the installation, and an administrator approving
this Runner — and neither is a release step.
