# Releasing the external Runner

For whoever cuts the release. Somebody installing the product wants
[AlgoJudge-Ops](https://github.com/AlgoJudge/AlgoJudge-Ops) instead, where this
is the `external-runner` profile.

**This one goes after `AlgoJudge-Runner`.** Not because a stack pulls the two
together — an installation that forwards nowhere never pulls this image at all
— but because this crate compiles against `aj-protocol`, which lives in that
repository. The pin is the first section below and the first thing to do.

## Where the version lives

**`Cargo.toml`, `[package] version`, one line**, and `Cargo.lock` beside it.
This is a single crate rather than a workspace, so there is nothing else to
keep in step.

**It is released on its own schedule.** The Server, the Client and the
sandboxing Runner are one set that a stack pulls by one tag; this is a fourth
image with its own repository, and an installation that does not forward
anywhere never pulls it at all.

## The Runner this release pins

**One thing in this repository names a Runner commit, and it is the
`aj-protocol` dependency.** It is written in two places, which must agree:

- `Cargo.toml`, `[dependencies] aj-protocol` — `git = "…/AlgoJudge-Runner"`
  with a `rev` of forty hex characters.
- `Cargo.lock` — the `source = "git+…?rev=<rev>#<rev>"` line under
  `name = "aj-protocol"`. Cargo writes it; do not hand-edit it.

Nothing else can express such a pin, and nothing else needs to. The two Runners
never speak to each other, so there is no runtime version to match: which
`algojudge-runner` image an installation runs is `AlgoJudge-Ops`'s
`RUNNER_TAG`, not this repository's. The one other shared constant is the Rust
base image digest, below, and that is a compiler rather than a release.

**Move the pin to the commit `AlgoJudge-Runner`'s tag points at**, not to
whatever `main` is on the day:

1. `git -C ../AlgoJudge-Runner rev-parse vX.Y.Z^{commit}` — that is the value.
2. Put it in `Cargo.toml` as `rev`, and say in the comment beside it which tag
   it is. `tag = "vX.Y.Z"` also resolves, but a tag can be moved and a revision
   cannot; the comment carries the name and the `rev` carries the guarantee.
3. `./x build`, which rewrites `Cargo.lock`. Commit both files.

It is done when all three are true: the forty characters in `Cargo.toml` are
what step 1 printed, the same forty appear twice in that `Cargo.lock` line, and
a second `./x build` leaves `Cargo.lock` untouched.

**Read the diff before believing it is a formality.** `aj-protocol` declares
its dependencies with `.workspace = true`, so it resolves against
`AlgoJudge-Runner`'s *workspace* `Cargo.toml` as well as its own — a version
range widened over there arrives here with the pin.

    git -C ../AlgoJudge-Runner diff <old-rev>..vX.Y.Z -- crates/aj-protocol Cargo.toml

**Where it stands on 2026-09-07.** The pin is `490d2e26038dccaf1228178b45205d78452a2cd9`,
an ancestor of `AlgoJudge-Runner`'s `release/0.1.0` (`77a20fd`). The
`crates/aj-protocol` tree is identical at the pin, at that branch and at `main`
— all three are `06b253380d6f712ebd3f8eaba9e15caee5a000e7` — and the only
change to that workspace manifest since the pin adds `libc`, which
`aj-protocol` does not use. So moving the pin onto the 0.1.0 commit compiles to
the same bytes. Do it anyway: what it buys is a lockfile that names the release
this was built against rather than a commit in the middle of one.

## The two Runners repeat each other, and the copy has drifted

Registration, approval, claiming, the lease, reporting, uploads, the long-poll
wait, the cache, backoff and shutdown exist twice — once in
`crates/aj-runner/src/run.rs` over there and once in `src/run.rs` here. The
shared *client* is `aj-protocol` and cannot drift; the loops around it can, and
have.

**Read the two side by side before every release** and settle each difference
as deliberate or as arrears. What follows was checked on 2026-09-07 against
`AlgoJudge-Runner` at `release/0.1.0`.

### What the sandboxing Runner has and this does not

None of these is a release blocker. All of them are arrears.

| | |
|---|---|
| `Retry-After` | `Error::retry_after()` is called nowhere here. Over there every wait goes through `how_long` (`crates/aj-runner/src/run.rs:147`), which takes the Server's number where it asks for longer than the local backoff. An operator asking for five minutes gets thirty seconds here |
| Maintenance windows | `Server::health()`, `Error::unavailable()` as a state of its own and `Error::in_maintenance()` are unused here. `wait_out` (`crates/aj-runner/src/run.rs:167`) reads `/health`, logs the operator's own words, and returns the moment the window ends. Here `unavailable()` is folded into `retryable()` (`src/run.rs:110`, `src/run.rs:132`) and in the claim loop reaches the catch-all `warn!` at `src/run.rs:322` |
| A revoked key | `Error::revoked()` is unused here, so a revocation exits with "the Server refused the registration" (`src/run.rs:117`) instead of saying the key is dead and a new registration is needed |
| The registration fingerprint | `Registered.fingerprint` is never compared with `Identity::fingerprint()` here. That comparison is what catches a public key re-encoded in transit, whose only other symptom is every later signature failing with nothing to explain it |
| The report retry | `send` (`src/run.rs:802`) is ten attempts backing off 2 s to 30 s, on **any** error including one that will never succeed. `report_with_retries` over there stops on `!e.retryable()` and bounds the whole retry by the lease it actually holds. At the shipped `AJ_Lease__RequestSeconds=1200` this gives up about five minutes into a twenty-minute lease |
| A tunable Server backoff | `AJ_Poll__MinSeconds` and `AJ_Poll__MaxSeconds` exist over there. Here the admission backoff is 2–60 s (`src/run.rs:73`) and the claim backoff 1–30 s (`src/run.rs:186`), both literals |
| What a landed report said | `ReportAccepted` is discarded here (`src/run.rs:806`). The other Runner logs `result_id`, `state` and `duplicate`, which is how a duplicate report is told from a first one |

### What this does differently on purpose

Leave these alone.

- `external: true` and `machine: None` at registration. It measures nothing, and
  the Server pairs work with workers on that flag by equality.
- No trials. `claim_trial` and `report_trial` are never called.
- A pool of up to `AJ_External__MaxPending` jobs, renewed together in
  `renew_everything` on the judge's own polling cycle — against one job and a
  background `Keeper` task over there. A held lease is renewed once per cycle,
  which is why `AJ_External__PollMaxSeconds` plus `AJ_Poll__WaitSeconds` must
  fit four times inside it (`src/config.rs:354`).
- `progress` after a forward, which the sandboxing Runner never calls: this one
  then waits on somebody else's service for up to fifteen minutes.
- A constant 60 s heartbeat rather than `AJ_Heartbeat__Seconds`. One process
  against one judge has no fleet to tune.
- Every held job is released on `SIGTERM`, not one.

## What a tag does

`.github/workflows/release.yml` runs on a pushed tag matching `v*`, refuses one
that does not point at a commit on `main`, refuses a name that is not
`v<major>.<minor>.<patch>[-prerelease]`, and publishes one image —
`ghcr.io/algojudge/algojudge-external-runner` — under `0.1.0`, `0.1`, `0` and
`latest`. A prerelease publishes its own tag alone. `linux/amd64` only.

Before pushing it checks two things about the image itself: that it **refuses to
start with nothing configured**, and that its two directories exist and belong to
`nonroot`. Both are properties an operator depends on and neither is visible from
the source alone.

**The package it creates is private, because this repository is**, and the
workflow cannot change that. Somebody with access to the organisation's packages
decides once whether it becomes public; `AlgoJudge-Ops/docs/INSTALL.md` counts it
among the packages an installation needs and says it is not like the other seven.
Publishing the image at all is that decision, and it is not made here.

## Before the tag

- [ ] **The commit being tagged is on `main`.** `release.yml` refuses a tag that
      is not, and CI runs on `main` and on pull requests into it and nowhere
      else — so a `release/x.y.z` branch has no CI run of its own however green
      it looks. On 2026-09-07 `release/0.1.0` was two commits ahead of `main`
      (`4ded646` and `73b68ea`, documentation only), and CI had run on neither. Merge the branch into `main`, let that run go green, and tag
      the merge.
- [ ] The `aj-protocol` pin names the commit of `AlgoJudge-Runner`'s own
      release. The section above says how, and how to tell.
- [ ] `Cargo.toml` says the version being released, and `Cargo.lock` agrees.
- [ ] `README.md` names that version under *The published image*.
- [ ] That commit's own CI run is green.
- [ ] `./x gate` — fmt, clippy with warnings as errors, a release build and the
      whole suite. Rust is not a prerequisite; `x` runs cargo in a container
      pinned by digest. Docker is.
- [ ] The two Runners have been read side by side, and every difference is on
      one of the two lists above.
- [ ] The `Dockerfile` base digest is the one intended, and **the same digest
      `AlgoJudge-Runner` pins**. Two Rust images a month apart is two compilers
      nobody chose. It is written in three files here — `Dockerfile`,
      `Dockerfile.toolchain`, `.github/workflows/ci.yml` — and in the same three
      there; all six must match.
- [ ] **Every image this repository pins has been looked at**, and what is
      behind is behind for a reason somebody wrote down. Four lines, three
      distinct images, in four files:

      | | where | how it moves |
      |---|---|---|
      | `rust@sha256:…` | `Dockerfile`, `Dockerfile.toolchain`, `ci.yml` | a digest, and the item above governs it |
      | `gcr.io/distroless/static-debian13:nonroot` | `Dockerfile` | a tag, and **the base of the image this repository publishes** |
      | `postgres:18` | `example-development-docker-compose.yaml` | a tag, major pinned on purpose — development only |

      ```sh
      grep -n '^FROM' Dockerfile Dockerfile.toolchain
      grep -rn 'image:' example-*.yaml .github/workflows/*.yml
      ```

      **A digest says what it is and never what it is behind**, so this is a
      question to ask the registry rather than the file. Record the answer and
      the date whether or not anything moves.
- [ ] Somebody has looked for advisories against `Cargo.lock`. **Nothing in this
      repository does it**: there is no `cargo audit` or `cargo deny` step in
      either workflow, none in `x`, no `deny.toml` and no Dependabot
      configuration. Until there is, it is a person running
      `./x install cargo-audit` and `./x audit`, and the date of that run is
      what a release can claim.
      `scraper` is the one direct dependency whose requested range sits a full
      minor behind what exists upstream — `0.20`, against `0.22`.
- [ ] `.env.example` has been **read**, not just tested. The suite compares it
      against the source, and the comparison is narrower than it sounds:
      `every_variable_the_config_reads_is_in_the_example_and_no_others` in
      `src/config.rs` reads `src/config.rs` as text and `.env.example` as text,
      and compares **names only**. Sample values and comments are checked by
      nobody. A commented-out line counts as listed. A key read from any file
      other than `src/config.rs` is invisible to it. And it matches a **closed
      list of sections** — `Server`, `Runner`, `Cache`, `External`, `Lease`,
      `Poll`, in `a_key` — so a key in a section not on that list is unchecked
      in both directions and looks checked. Adding a section to the
      configuration means adding it there too. And the whole check is `AJ_`-prefixed: `RUST_LOG`
      (`src/main.rs:14`, the default variable behind `EnvFilter::try_from_default_env`) and `HOSTNAME` (`src/config.rs:216`) are read by this
      Runner, listed by nothing, and invisible to it in both directions.
- [ ] Nothing documents `AJ_Uva__*`. The prefix became `AJ_External__*` on
      2026-08-31; `uva@1`, `props.uva.problemNumber` and `onlinejudge.org` did
      not change, and are not what this is looking for. **The sweep for
      `AJ_Uva__`, `Runner-UVa` and `algojudge-runner-uva` came back empty on
      2026-09-07**, tracked files and working tree alike.
- [ ] `git ls-files` lists `.env.example` and no `.env`. A real `.env` in the
      working tree is expected and ignored — do not open it, and do not let it
      into a commit or a log.
- [ ] Run it end to end at least once against a Server, in two passes.

      **The suites first, with no Runner attached to that Server.** Bring the
      stack up without this service — `up -d --wait postgres server` — and run
      them one at a time:

      ```sh
      AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1         ./x test -- --include-ignored --test-threads=1
      ```

      Both halves of that command are load-bearing, and each was learned the
      hard way on 2026-09-07. **A Runner in the stack competes for the queue**:
      `claim_lease` waits for a job in an unbounded loop and waited twenty
      minutes for one this Runner had taken — which it had been allowed to take,
      because the test approves every Runner it finds — and `stack` asserts a
      fresh submission is `queued`, which a long-polling Runner makes `running`
      within milliseconds. **And `--test-threads=1`**: two of these turn external
      judging on through `PUT /instance`, and in parallel the second gets a 409
      from the Server's optimistic concurrency, correctly.

      `lease` alone takes about three minutes, because it waits out real
      renewal cycles.

      **Then the manual path**, which sends a **real** submission to a real
      service and needs the account and whoever owns it. Start this service only
      once the submission you mean to send exists: the queue survives the
      service being down, so starting it after a run of the suites forwards
      everything that accumulated. On 2026-09-07 that was eight real
      submissions where one was intended, and they stay on the account.

      Two things a reset stack needs and a running one does not.
      **`docker compose up -d` rather than `start`**: after `down -v` there is
      no container to start, and `start` says so in a way that is easy to read
      as success. **And the Runner needs approving again** — `down -v` takes the
      identity volume with it, so it registers under a new key and waits. The
      suites approve every Runner they find; nobody approves this one.

      What a fresh development database is not, is empty: the Server seeds one,
      and four of its jobs sit queued. They are `standard-io@1`, so this Runner
      never touches them.
- [ ] **The credential in your own `.env` is not in the commit.** `.env.example`
      gives no value to either secret, and nothing else should.
- [ ] The documentation still describes this repository: every `.md` here,
      `docs/README.md` and `docs/UVA.md` included, and the comments in
      `Dockerfile`, `Dockerfile.toolchain`, `example-development-docker-compose.yaml`
      and `x`. The comments in this repository carry dates and measurements, and
      they age like the code does. **Six things were wrong on 2026-09-07 and are
      not fixed by this file**: `README.md:202` states the four-times rule
      against `AJ_External__PollMaxSeconds` alone when the code counts
      `poll_max + poll_wait`; `README.md`'s list of refusals gives four and the
      code has eight; `README.md` never names `AJ_Poll__WaitSeconds` at all;
      `docs/README.md:3` claims the README carries every `AJ_External__*`
      variable and `LongPollEnabled` is missing from it; the *Two sections*
      paragraph at `README.md:160` names three prefixes of five;
      `example-development-docker-compose.yaml:11` counts thirteen unignored
      tests where there are sixteen.
- [ ] The two `docs.algojudge.pl` links in `README.md` —
      `/en/runner/external/` and `/en/install/external-runner/` — have pages
      behind them in `AlgoJudge-Docs`. **The site has no DNS record yet**, so
      they resolve to nothing on the day of the release whatever the repository
      says.

## After the tag

Nothing here depends on this image, and it depends on nothing here: an
installation adds the `external-runner` profile when it wants one, and
`AlgoJudge-Ops` pulls it by the moving major `0` —
`EXTERNAL_RUNNER_TAG` in its `compose.yaml`, defaulted to `0`.

Two things outside this repository decide whether it is ever handed work —
external judging being on for the installation, and an administrator approving
this Runner — and neither is a release step.
