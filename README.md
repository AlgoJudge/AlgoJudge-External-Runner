# AlgoJudge-Runner-UVa

AlgoJudge is open-source, self-hosted software for programming contests and
courses, with automatic evaluation of submitted solutions.

This is a Runner that does not judge anything. It forwards `uva@1` submissions
to [onlinejudge.org](https://onlinejudge.org), waits for the archive to decide,
and reports the archive's verdict back to AlgoJudge.

**The verdict is somebody else's opinion**, and every screen that shows it says
so. This Runner runs no code, has no sandbox, and measures nothing.

## What makes it different from `AlgoJudge-Runner`

| | `AlgoJudge-Runner` | here |
|---|---|---|
| Where work is judged | in a sandbox it starts | on a service it does not run |
| How long a job is held | seconds to minutes | up to fifteen minutes, waiting |
| Registers as | `external: false` | **`external: true`** |
| Trials | measures them | refuses them |
| Runtime image | `Dockerfile`, distroless | `Dockerfile`, distroless and smaller |

**`external: true` is not a detail.** The Server pairs a problem with a Runner on
that flag and the problem's own, by equality — so a Runner that forwards and does
not say so is handed nothing at all, and from a log that is indistinguishable
from an empty queue. An end-to-end run lost ten minutes to exactly that before
the field existed in the protocol.

## Building and testing

Rust is not a prerequisite. `./x` runs cargo in a container pinned by digest:

    ./x gate        fmt, clippy -D warnings, release build, the whole suite
    ./x test        the suite alone
    ./x run --release

Everything in the suite runs offline. The tests against the archive and uHunt
drive a recorded stand-in started in process, so **the live archive is never a
test dependency** — which is why CI needs no services and no secrets.

## Running it in a container

`Dockerfile` builds a static musl binary into `distroless/static`, about 11 MB
with no shell and no package manager. It is **smaller than the sandboxing
Runner's on purpose**: that one holds the container runtime's socket and starts
sibling containers, and this one starts nothing — so there is no socket, no
cgroups, no scratch directory and no cache. The only state is the identity key,
in `/var/lib/algojudge-runner-uva`, which is meant to be a volume: losing it
costs a re-registration and an administrator's approval.

`example-uva-development-docker-compose.yaml` raises PostgreSQL, a Server built
from the sibling checkout, and this Runner:

    docker compose -f example-uva-development-docker-compose.yaml up -d --build --wait
    AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1 ./x test -- --include-ignored
    docker compose -f example-uva-development-docker-compose.yaml down -v

**This is the stack §"Running it end to end" below asks for.** Port 8098 rather
than 8080, so it stands beside the Server's own development stack and
`AlgoJudge-Runner`'s without either taking the other's port.

Two things it cannot do for you, and each stops the queue dead with the Runner
looking perfectly healthy: **turning external judging on** — the Server ships
with it off and hands out no external work at all while it is — and **approving
this Runner**, which is the trust decision the whole design rests on.

**`.env` is passed to the container, and it was written for `./x`.** `./x` mounts
the source at `/work`, so the key path in it points inside the source tree, which
the image neither should nor — running as `nonroot` — can write. The compose file
states `AJ_Runner__KeyPath` in `environment:`, which takes precedence, and that
is what lets one `.env` serve both.

## Configuration

Every variable is `AJ_`-prefixed, the same convention the Server reads.
`.env.example` lists them all and gives **no** value to either secret; `.env` is
git-ignored and is what `./x` passes to the container as a file rather than on a
command line, because an argument lands in the shell history and the process
list.

**`AJ_Runner__Tags` names the pools this Runner belongs to**, comma-separated.
The Server pairs a Runner with work when the two tag lists **share at least one**
entry, and an empty list on either side means `default` — so naming a pool takes
this Runner out of the general queue as surely as it puts it into a reserved one.
`docs/specs/RUNNER_ROUTING.md` in the workspace owns the rule.

**It is a seed, not a setting.** The Server reads it at the **first**
registration and never again; from then on the operator owns it in the panel.
It exists so a room of machines is deployed from one file rather than tagged one
at a time, and it stops there: a Runner that could re-declare its tags on restart
would put itself into an examination's pool with nobody having approved it.
Changing the variable later changes nothing, deliberately.

Two numbers are checked against each other at start-up rather than discovered an
hour later:

- **`AJ_Lease__RequestSeconds` must exceed `AJ_Uva__PendingTimeoutSeconds`.**
  Otherwise the Server reclaims the job while this Runner is still waiting on the
  archive, and the next Runner to claim it submits the same solution again.
- **`AJ_Uva__PollMaxSeconds` must fit four times inside the lease.** A held lease
  is renewed on the polling cycle, so slowing the polling down to be polite to
  uHunt slows the renewing down with it — and a lease that expires between two
  renewals is the same double submission by another route.

## Running it end to end

The whole path, against a throwaway Server. **The last step sends a real
submission to a real service**, so it needs a robot account and a decision from
whoever owns it.

1. **A Server.** Bring one up from `AlgoJudge-Server` and turn external judging
   on — it ships **off**, and while it is off no external work is handed out at
   all.

2. **A problem.** Fetch the statement through the Server, because the archive
   sends no `Access-Control-Allow-Origin` and a browser cannot read it:

       POST /files/fetch   {"url": "https://onlinejudge.org/external/1/100.pdf"}
       POST /problems      {"slug":"UVa-100", "type":"uva@1", "external":true}
       POST /problems/{id}/versions
           {"statements":[…], "props":{"type":"uva@1","uva":{"problemNumber":100}}}

   **`props` on the version, not `config`** (2026-08-22). It says *which problem
   this is*, which is a fact about the problem rather than about one activity's
   use of it — so it is written once at import and every assignment inherits it,
   instead of the same number being copied wherever the problem is attached.

   **Without it this Runner refuses the job before anything leaves**, and says
   so by name. A problem imported before that date kept its number on the
   version's `config`, which no longer exists; the message says that too, because
   the symptom is identical to a problem nobody configured at all.

   There is no language map to write any more: `uva@1` defines its own six.

3. **An activity**, a round that has opened, the problem attached, somebody
   enrolled. **Attach after the configuration is right**: the assignment pins the
   problem version at the moment it is attached, deliberately, so publishing a
   correction afterwards does not change what a running round is judged against.

4. **This Runner**, pointed at that stack, then approved in the manager panel:

       AJ_Server__BaseUrl=http://host.docker.internal:8098/api/v1 \
       AJ_Runner__ProblemTypes=uva@1 RUST_LOG=info ./x run --release

   Both of those are forwarded from the host by `./x`, which is how a Runner is
   pointed somewhere other than the stack its `.env` names.

5. **Submit**, and watch:

       INFO  handed to onlinejudge.org  job=… sid=31255986
       INFO  resolved the archive account  uid=…

   The verdict arrives on the polling interval and is reported to the Server as
   an ordinary result.

**Every real submission stays on the account for ever.** Use a solution written
to be wrong: it keeps the account's solved count honest, and it avoids the
question of what a duplicated *accepted* solution does, which nobody has measured.

## The six languages

What onlinejudge.org offers, and what to call each of them here. **The problem
type defines this list** (2026-08-22) — it is `src/language.rs`, and this table
is that file written out.

It was in each problem's configuration until then, so that a language the
archive adds would be a re-published problem rather than a release of this
Runner. What that missed is that the list belongs to *the archive*, which every
`uva@1` problem shares: holding it per problem meant writing the same six
numbers into every import, and an import that wrote none produced a problem
nobody could submit to — the failure a live run found on 2026-08-16.

| Id | Label | The archive's own | № |
|---|---|---|---|
| `c89-gcc` | C89 / ANSI C (GCC 5.3.0) | ANSI C 5.3.0, `-ansi -O2 -lm -lcrypt -DONLINE_JUDGE` | 1 |
| `java8` | Java 8 (OpenJDK 1.8.0) | JAVA 1.8.0 | 2 |
| `cpp98-gcc` | C++98 (GCC 5.3.0) | C++ 5.3.0, `-O2 -lm -lcrypt -DONLINE_JUDGE` | 3 |
| `pascal-fpc` | Pascal (Free Pascal 3.0.0) | PASCAL 3.0.0 | 4 |
| `cpp11-gcc` | C++11 (GCC 5.3.0) | C++11 5.3.0, `-std=c++11 -O2 …` | 5 |
| `python3` | Python 3 (CPython 3.5.1) | PYTH3 3.5.1 | 6 |

**Three of these ids are the same ids `standard-io@1` uses** — `c89-gcc`,
`cpp11-gcc` and `python3` — deliberately, so one screen can resolve a label
whichever type produced a submission. The three that are not (`cpp98-gcc`,
`java8`, `pascal-fpc`) name toolchains this project does not run itself; that is
the point of forwarding.

**The labels are not `standard-io@1`'s, and must not be.** The compilers here are
the archive's, pinned at the archive's versions: `cpp11-gcc` there is GCC 14 with
our flags, and here it is GCC 5.3.0 with UVa's. Showing "C++11 (GCC)" in both
places would tell a participant the two were built by the same compiler.

## Known gaps

- **The long-poll trigger is not built.** The Runner says so at every start.
  Verdicts arrive on the interval net alone, which is slower but not wrong.
- **One of the six numbers has been watched work.** `5` is the id seen accepted.
  The other five are the archive's own form values and are written down below,
  but nobody here has submitted through them, and that distinction is the whole
  of this gap.
- **Two of five behaviours around the protocol client are covered; three are
  not.** *Said "two of four … two are not" until 2026-08-30: it was two of four
  when it was written on 2026-08-23, and a fifth behaviour — the lease asked for
  being the lease granted — was added to the covered list in the same edit
  without the count following it. Recounted from the two lists below.*

  The conformance cases are often described as owed by this repository, and on
  inspection that is imprecise. Those cases exercise **an implementation of the
  contract**, and this Runner does not have one: it reaches the Server only
  through `aj-protocol`, the same crate `AlgoJudge-Runner` uses, whose suite
  already drives that code against a live Server. Porting them here would test
  the same lines twice.

  What is worth covering is what this Runner does *around* that client. All of it
  needs a harness that signs in to a Server, publishes a problem and submits to
  it — **`tests/stack.rs` is that harness**, added 2026-08-22, and it is a Server
  session driven the way a person would drive it: a cookie, never a Runner token.

  Covered:

  - **lease renewal actually firing** — `tests/lease.rs` holds a job past the
    deadline it was granted and asks the Server whose it is. It fails when
    `renew_everything()` is deleted, which it did not until `AlgoJudge-Server`
    stopped replacing a claimed lease with its own default on the first progress
    report;
  - **the lease this Runner asks for is the lease it gets** — `tests/claim_lease.rs`,
    offline on the wire format and live against a Server, because those two were
    right and wrong respectively at the same time.

  Not covered:

  - **a lost lease dropping the job silently** — reporting against a stale lease
    would overwrite whoever holds it now, and the Server's idempotency would not
    help, because it keys on the token this Runner no longer has;
  - **an archive that is unreachable reported as infrastructure rather than as a
    wrong answer** — the distinction the whole module rests on;
  - **serialised submission**, which is a correctness requirement here rather
    than politeness: two in flight at once cannot be told apart on the way back.
    This one needs the real archive and fresh submissions from the robot account.

  All of these are `#[ignore]`d and need `AJ_TEST_SERVER`, so **CI runs none of
  them**. A regression in lease renewal will not redden a pull request; somebody
  has to run it. **Half of that is now closed**:
  `example-uva-development-docker-compose.yaml` stands the Server up, so running
  them is one command rather than an afternoon. What is still owed is the CI step
  that calls it.
