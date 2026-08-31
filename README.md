# AlgoJudge-External-Runner

AlgoJudge is open-source, self-hosted software for programming contests and
courses, with automatic evaluation of submitted solutions.

This is a Runner that does not judge anything. It claims jobs of an external
problem type, forwards the solution to the judging system that owns that type,
waits for that system to decide, and reports its verdict back to AlgoJudge.

**The verdict is somebody else's opinion**, and every screen that shows it says
so. This Runner runs no code, has no sandbox, and measures nothing.

## Integrations

**One exists: UVa Online Judge**, serving the problem type `uva@1` against
[onlinejudge.org](https://onlinejudge.org). It is `src/uva/`.

Everything the loop needs from a judging system is declared as one trait in
[`src/integration.rs`](src/integration.rs) — what the judge is called, which
problem type it serves, which languages it accepts, how a submission is handed
over, and how an answer is read. `src/run.rs` is written against that trait and
names no archive, so a second integration is a module beside `src/uva/` and one
arm in `main`, not a fork of the loop.

A second integration would implement:

| | |
|---|---|
| `problem_type` / `name` | the type it serves, and what a result document calls it |
| `languages` | what it accepts, and what to label each one |
| `read` | which problem a version's `props` names |
| `problem` | the judge's internal id for that problem |
| `submit` | hand a solution over, return the judge's id for it |
| `answers` | one request for everything still outstanding |
| `id_of` / `problem_of` / `outcome` / `evidence` | how one answer is read |
| `details` / `details_of_failure` | the result documents its renderer expects |

**One process serves one judge.** There is one endpoint and one account in the
configuration, deliberately: two judging systems are two deployments of this
Runner, each with its own problem types and its own pools, which is how Runners
already scale.

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
from an empty queue.

## Building and testing

Rust is not a prerequisite. `./x` runs cargo in a container pinned by digest:

    ./x gate        fmt, clippy -D warnings, release build, the whole suite
    ./x test        the suite alone
    ./x run --release

Everything in the suite runs offline. The tests against a judging system drive a
recorded stand-in started in process, so **no live judge is ever a test
dependency** — which is why CI needs no services and no secrets.

## Running it in a container

`Dockerfile` builds a static musl binary into `distroless/static`, about 11 MB
with no shell and no package manager. It is **smaller than the sandboxing
Runner's on purpose**: that one holds the container runtime's socket and starts
sibling containers, and this one starts nothing — so there is no socket, no
cgroups and no scratch directory.

Two directories, and the difference between them matters. The identity key is in
`/var/lib/algojudge-external-runner` and is meant to be a volume: losing it costs
a re-registration and an administrator's approval. A submission's source is
cached in `/var/cache/algojudge-external-runner` (`AJ_Cache__Path`, bounded by
`AJ_Cache__MaxBytes`), and losing that costs one download. **There is no *package* cache** — an external problem
has none, because its whole configuration travels on the job — which is not the
same as there being no cache, and this said the second thing until 2026-08-31.

`example-development-docker-compose.yaml` raises PostgreSQL, a Server built from
the sibling checkout, and this Runner:

    docker compose -f example-development-docker-compose.yaml up -d --build --wait
    AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1 ./x test -- --include-ignored
    docker compose -f example-development-docker-compose.yaml down -v

**This is the stack §"Running it end to end" below asks for.** Port 8098 rather
than 8080, so it stands beside the Server's own development stack and
`AlgoJudge-Runner`'s without either taking the other's port.

Two things it cannot do for you, and without either the queue stays empty:
**turning external judging on** — the Server ships with it off and hands out no
external work while it is — and **approving this Runner**, which is the trust
decision the whole design rests on.

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

**Two sections, and the split is the point.** `AJ_Server__*`, `AJ_Runner__*` and
`AJ_Lease__*` are this Runner's own and mean the same thing whatever it forwards
to. `AJ_External__*` is the judging system it forwards to:

| Variable | |
|---|---|
| `AJ_External__Judge` | which integration to run. `uva` is the only one built, and the default |
| `AJ_External__BaseUrl` | where submissions are posted |
| `AJ_External__ApiBaseUrl` | where answers are read, when that is a different service. For UVa it is uHunt |
| `AJ_External__Username`, `AJ_External__Password` | the robot account. **Secrets**, and they have no default |
| `AJ_External__PollMinSeconds`, `PollMaxSeconds`, `PollEscalateAfterSeconds` | how often the judge is asked |
| `AJ_External__SubmitMinIntervalSeconds` | the gap between two submissions |
| `AJ_External__PendingTimeoutSeconds` | how long an unanswered submission is waited for |
| `AJ_External__MaxPending` | how many may be outstanding at once |

An unknown judge is refused at start-up, by name and with the list of what this
build knows.

**`AJ_Runner__ProblemTypes` may be left unset**, and usually is: silence declares
whatever the integration serves. Set it only to narrow or widen that
deliberately.

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

- **`AJ_Lease__RequestSeconds` must exceed `AJ_External__PendingTimeoutSeconds`.**
  Otherwise the Server reclaims the job while this Runner is still waiting on the
  judge, and the next Runner to claim it submits the same solution again.
- **`AJ_External__PollMaxSeconds` must fit four times inside the lease.** A held
  lease is renewed on the polling cycle, so slowing the polling down to be polite
  to somebody else's service slows the renewing down with it — and a lease that
  expires between two renewals is the same double submission by another route.

## Running it end to end

The whole path, against a throwaway Server. **The last step sends a real
submission to a real service**, so it needs a robot account and a decision from
whoever owns it. The steps below use the UVa integration, because it is the one
that exists.

1. **A Server.** Bring one up from `AlgoJudge-Server` and turn external judging
   on — it ships **off**, and while it is off no external work is handed out at
   all.

2. **A problem.** Fetch the statement through the Server, because the archive
   sends no `Access-Control-Allow-Origin` and a browser cannot read it:

       POST /files/fetch   {"url": "https://onlinejudge.org/external/1/100.pdf"}
       POST /problems      {"slug":"UVa-100", "type":"uva@1", "external":true}
       POST /problems/{id}/versions
           {"statements":[…], "props":{"type":"uva@1","uva":{"problemNumber":100}}}

   **`props` on the version, not `config`.** It says *which problem this is*,
   which is a fact about the problem rather than about one activity's use of it,
   so it is written once at import and every assignment inherits it.

   **Without it this Runner refuses the job before anything leaves**, and says so
   by name. There is no language map to write: `uva@1` defines its own six.

3. **An activity**, a round that has opened, the problem attached, somebody
   enrolled. **Attach after the configuration is right**: the assignment pins the
   problem version at the moment it is attached, deliberately, so publishing a
   correction afterwards does not change what a running round is judged against.

4. **This Runner**, pointed at that stack, then approved in the manager panel:

       AJ_Server__BaseUrl=http://host.docker.internal:8098/api/v1 \
       RUST_LOG=info ./x run --release

   That is forwarded from the host by `./x`, which is how a Runner is pointed
   somewhere other than the stack its `.env` names.

5. **Submit**, and watch:

       INFO  handed over  job=… judge=onlinejudge.org sid=31255986
       INFO  resolved the archive account  uid=…

   The verdict arrives on the polling interval and is reported to the Server as
   an ordinary result.

**Every real submission stays on the account for ever.** Use a solution written
to be wrong: it keeps the account's solved count honest, and it avoids the
question of what a duplicated *accepted* solution does, which nobody has measured.

## The UVa integration

### The six languages

What onlinejudge.org offers, and what to call each of them here. **The problem
type defines this list**, because it belongs to the archive rather than to any
one problem: it is `src/uva/language.rs`, and this table is that file written out.

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

**Only number 5 has been watched work.** A real submission was accepted under it
on 2026-08-16. The other five are read off the archive's form and nobody here has
submitted through them.

### Two services, one judge

Submitting is an HTML form flow behind a session cookie on `onlinejudge.org`;
reading a verdict is a JSON API on `uhunt.onlinejudge.org`. That is why the
configuration has both a `BaseUrl` and an `ApiBaseUrl`, and why `src/uva/` has
`site.rs` beside `uhunt.rs`.

## Related repositories

- [AlgoJudge-Runner](https://github.com/AlgoJudge/AlgoJudge-Runner) — the
  sandboxing Runner, and the `aj-protocol` crate this one consumes over Git,
  pinned to a revision in `Cargo.toml`
- [AlgoJudge-Server](https://github.com/AlgoJudge/AlgoJudge-Server) — jobs,
  problems, results, and the switch that turns external judging on
- [AlgoJudge-Ops](https://github.com/AlgoJudge/AlgoJudge-Ops) — the production
  Compose stack

## Contributing

Open an issue saying what you expected, what happened, and how to reproduce it.
Or open a pull request against `main`: one subject per pull request, with a note
on what changes and why.

By contributing you agree that your work is licensed under the terms below.

## License

This project is licensed under the MIT License.
See [LICENSE](LICENSE).

Authors are listed in [AUTHORS.txt](AUTHORS.txt).
