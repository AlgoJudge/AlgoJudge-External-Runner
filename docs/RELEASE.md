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
whatever `main` is at the time:

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

**Where it stands.** The pin is `640cfba6c24ee477d9e2a6cdb2ea0b954e4cd862`,
which is `AlgoJudge-Runner`'s `v0.2.0`.

**The move is not a formality, and the size of the diff is not the measure of
it.** Between 0.1.0 and 0.2.0 the crate gained a cache several Runners share —
an entry is a directory under `packages/` holding the bytes and everything built
from them, behind a `flock`, which is what brought `libc` in. None of it reaches
this Runner's calls: `Cache::new`, `Cache::sweep`, `Cache::fetch`,
`Entry::path` and every `Server` method keep their signatures, and
`Server::download_verified` is an addition. **Prove that with a build rather
than with a reading**, because the reading is what a widened range slips past.

**An entry written under the older layout is reclaimed, not collided with.**
Those bytes sit at `<root>/xx/yy/zz/<fileId>`; an entry now lives under
`<root>/packages/`, which the older code never wrote to, and `Cache::sweep` —
called at start in `src/main.rs` — removes them. An upgrade re-downloads what it
had cached, which is what a cache is for.

## The two Runners repeat each other

Registration, approval, claiming, the lease, reporting, uploads, the long-poll
wait, the cache, backoff and shutdown exist twice — once in
`crates/aj-runner/src/run.rs` over there and once in `src/run.rs` here. The
shared *client* is `aj-protocol` and cannot drift; the loops around it can.

**Read the two side by side before every release** and settle each difference
as deliberate or as arrears. **Nothing is outstanding**: every wait goes through
`how_long`, which prefers the Server's `Retry-After` to the local backoff; a
maintenance window is waited out through `wait_out` against `/health` rather
than folded into a generic retry; a revoked key is named as a dead key;
`Registered.fingerprint` is compared with `Identity::fingerprint()`, which is
what catches a public key re-encoded in transit; the report retry is bounded by
the lease rather than by a count, and stops on an error that will never succeed;
`AJ_Poll__MinSeconds` and `AJ_Poll__MaxSeconds` are read rather than written as
literals; and `ReportAccepted.duplicate` is read, which is how a duplicate
report is told from a first one.

### What this does differently on purpose

Leave these alone.

- `external: true` and `machine: None` at registration. It measures nothing, and
  the Server pairs work with workers on that flag by equality.
- No trials. `claim_trial` and `report_trial` are never called.
- A pool of up to `AJ_External__MaxPending` jobs rather than one, claimed and
  released in batches, and every one of them given back on `SIGTERM`.
- **Renewal on a cadence of its own** — a quarter of the granted lease, the rule
  `keeper.rs` uses over there — and deliberately unrelated to how often the
  judge is polled. Those two are politeness toward somebody else's service; this
  is whether the Server still believes this Runner is alive. Coupling them back
  together fails `a_slow_poll_interval_no_longer_touches_the_lease`.
- `progress` after a forward, which the sandboxing Runner never calls: this one
  then waits on somebody else's service for up to fifteen minutes.
- A constant 60 s heartbeat rather than `AJ_Heartbeat__Seconds`. One process
  against one judge has no fleet to tune.
- **A submission's source goes through the cache here and through nothing
  there.** The sandboxing Runner caches packages, which many submissions share,
  and takes a submission's own file with `Server::download_verified`, which
  keeps nothing. An external problem has no package — its whole configuration
  travels on the job — so here the cache holds sources, and that is what
  `AJ_Cache__MaxBytes` is sized for.

## What a tag does

`.github/workflows/release.yml` runs on a pushed tag matching `v*`, refuses one
that does not point at a commit on `main`, refuses a name that is not
`v<major>.<minor>.<patch>[-prerelease]`, and publishes one image —
`ghcr.io/algojudge/algojudge-external-runner` — under `<major>.<minor>.<patch>`,
`<major>.<minor>`, `<major>` and `latest`. A prerelease publishes its own tag
alone and moves none of the others. `linux/amd64` only.

Before pushing it checks two things about the image itself: that it **refuses to
start with nothing configured**, and that its two directories exist and belong to
`nonroot`. Both are properties an operator depends on and neither is visible from
the source alone.

**The package is public**, and an installation can pull it without
authenticating. That is a one-time decision somebody with access to the
organization's packages made, not something the workflow sets: a package created
by a first push is private whatever the repository is, and no token here can
change it. It matters only when a release publishes an image name that did not
exist before. Read it back rather than assuming, and anonymously — `gh api
.../packages` needs a `read:packages` scope the release token does not carry:

```sh
token=$(curl -s "https://ghcr.io/token?scope=repository:algojudge/algojudge-external-runner:pull&service=ghcr.io" \
  | python -c "import json,sys; print(json.load(sys.stdin)['token'])")
curl -s -H "Authorization: Bearer $token" \
  "https://ghcr.io/v2/algojudge/algojudge-external-runner/tags/list"
```

## Before the tag

- [ ] **The commit being tagged is on `main`.** `release.yml` refuses a tag that
      is not, and CI runs on `main` and on pull requests into it and nowhere
      else — so a `release/x.y.z` branch has no CI run of its own however green
      it looks, and a branch carrying nothing but documentation is no exception.
      Merge the branch into `main`, let that run go green, and tag the merge.
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

      **Asked on 2026-09-20, for 0.2.0.** The pinned Rust is **1.97.1**
      (`sha256:3b287904…`); `rust:slim` is **1.98.1** and resolves to
      `sha256:f47a8de2…`, so the pin is one minor and one patch behind.
      **Not moved, and deliberately**: the digest is shared with
      `AlgoJudge-Runner` across six files in two repositories, that repository
      released 0.2.0 on 1.97.1 the same day, and moving it here alone would
      leave the two compiling against different compilers — the one thing the
      item above exists to prevent. It moves in all six at once or not at all.
      `gcr.io/distroless/static-debian13:nonroot` is current.
- [ ] Somebody has looked for advisories against `Cargo.lock`. **No workflow
      does it**: there is no `cargo audit` or `cargo deny` step in either
      workflow, none in `x`, and no `deny.toml`. `.github/dependabot.yml` raises
      pull requests for cargo, docker and actions on a weekly schedule with a
      seven-day cooldown — and it ignores `aj-protocol` and `rust` by name,
      because both are pins that move deliberately — but a Dependabot pull
      request is an upgrade offer, not an advisory scan: it says nothing about
      what is already in the lock. **So the date of the last hand-run is the
      whole of the coverage**, and it is a person running
      `./x install cargo-audit --locked` and then `./x audit`.

      **Chain it.** `cargo audit` exits non-zero on a finding, so reading the
      output is not the check.

      **Run it before the version bump**, and where it finds something, measure
      the fix before applying it — `./x update --dry-run -p <crate>` says how
      many packages move. A patch that closes an advisory inside a range
      `Cargo.toml` already allows is taken during a release; a major that is
      failing its own checks is not. **Re-run `./x gate` afterwards**: the lock
      changed, so the earlier green is about other bytes.

      **Run on 2026-09-20 for 0.2.0**: 245 crate dependencies against 1251
      advisories, **one found** — `RUSTSEC-2026-0285`, published 2026-09-14,
      TLS 1.3 handshake messages accepted across encryption level boundaries,
      medium. `rustls` 0.23.44 reaches this Runner through `reqwest`, and it is
      the TLS it speaks both to the Server and to the judging service. Closed by
      `0.23.45`, a patch inside the existing range.
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
      2026-09-07**, tracked files and working tree alike, and again on
      2026-09-08 — where the only hits are this checklist item describing the
      sweep.
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
      as success. **And the Runner needs approval again** — `down -v` takes the
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
      they age like the code does.

      **Counts in prose are what rots first.** A sentence naming how many
      refusals, variables or tests there are is wrong the moment one is added,
      and nothing fails when it happens. Prefer the command that answers —
      `./x test -- --list` rather than a number — and where a count has to be
      written, recompute it here rather than trusting the sentence.
- [ ] The two `docs.algojudge.pl` links in `README.md` —
      `/en/runner/external/` and `/en/install/external-runner/` — answer. The
      site is published, so this is a request rather than a reading of
      `AlgoJudge-Docs`:

      ```sh
      for u in https://docs.algojudge.pl/en/runner/external/ \
               https://docs.algojudge.pl/en/install/external-runner/; do
        curl -sL -o /dev/null -w "$u %{http_code}\n" "$u"
      done
      ```

## Cutting the tag

The tag is what publishes; nothing that lands on `main` reaches the registry on
its own. Three things are true before it is cut, and each is read rather than
assumed:

```sh
git merge-base --is-ancestor <sha> origin/main              # it is on main
gh run list -R AlgoJudge/AlgoJudge-External-Runner --commit <sha>  # green
git tag --list                                              # the name is free
```

**Its own run.** A later green run on `main` is evidence about a later commit,
and a release branch has no run at all.

The tag is annotated, and the message names the product:

```sh
git tag -a v<version> -m "AlgoJudge External Runner <version>" <sha>
git push origin v<version>
```

That push starts `.github/workflows/release.yml`, which takes about **1 m 40 s**.
Watch it — `gh run watch <id>` — rather than assuming it.

**Two things have no undo.** The run is never canceled: `cancel-in-progress` is
`false` here because a run interrupted between two `docker push` calls leaves a
version half in the registry. And **deleting a tag unpublishes nothing** — the
images of a tag deleted from `AlgoJudge-Runner` in August 2026 are still in
GHCR. The name is checked before the push or not at all.

Then the GitHub Release, which no workflow creates — `release.yml` holds
`contents: read`:

```sh
gh release create v<version> -R AlgoJudge/AlgoJudge-External-Runner \
  --title "<version>" --notes-file <file>
```

The title is the bare version, no `v`; `--prerelease` when the version carries
one.

**A release body is not a file in this repository.** GitHub renders a single
newline as a line break, so each paragraph is written as one long line.

## After the tag

Nothing here depends on this image, and it depends on nothing here: an
installation adds the `external-runner` profile when it wants one, and
`AlgoJudge-Ops` pulls it by the moving major `0` —
`EXTERNAL_RUNNER_TAG` in its `compose.yaml`, defaulted to `0`.

Two things outside this repository decide whether it is ever handed work —
external judging being on for the installation, and an administrator approving
this Runner — and neither is a release step.
### The public website states this component's version

`algojudge.pl` prints **`External-Runner v<version>`** in four places — a card badge and a
roadmap item, in each of `src/content/pl.json` and `src/content/en.json` of
`AlgoJudge-Website`. A release makes all four wrong.

**`AlgoJudge-Website` has no CI.** Its fifteen tests run only when somebody types
`npm test`, so nothing reports the mismatch.

`AlgoJudge-Website/tests/content.test.mjs` pins the literal in **three** places,
so correcting the content turns that suite red and the test needs the same edit:
`:152` asserts the roadmap mentions `0.1.0` at all, `:155` selects the released
repositories by `badge === "v0.1.0"`, and `:161` loops over five hard-coded
component names asserting `<Component> v0\.1\.0`.

**The shape is what is wrong, not only the literal.** That loop asserts all five
components carry one version, and they do not: the five are released
independently and already sit on three different numbers. A test that cannot
express that is asserting a world the project left.

The correction is `/website-sync` in the workspace. This runbook's step is to
record that it is owed.
