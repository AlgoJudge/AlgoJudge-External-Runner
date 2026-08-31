# AlgoJudge-External-Runner

AlgoJudge is open-source, self-hosted software for programming contests and
courses, with automatic evaluation of submitted solutions.

This is a Runner that does not judge anything. It claims jobs of an external
problem type, forwards the solution to the judging system that owns that type,
waits for that system to decide, and reports its verdict back to AlgoJudge. It
runs no code, has no sandbox, and measures nothing.

**The verdict is somebody else's opinion.** The compilers, the tests, the limits
and the judgement belong to the judging system, not to the installation that
shows the result.

## Integrations

**One exists: UVa Online Judge**, serving the problem type `uva@1`. It is
`src/uva/`, and it is documented in [`docs/UVA.md`](docs/UVA.md) — its languages,
its two addresses and what a problem of its type carries.

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
`AJ_Cache__MaxBytes`), and losing that costs one download. **There is no
*package* cache** — an external problem has none, because its whole configuration
travels on the job.

`example-development-docker-compose.yaml` raises PostgreSQL, a Server built from
the sibling checkout, and this Runner:

    docker compose -f example-development-docker-compose.yaml up -d --build --wait
    AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1 ./x test -- --include-ignored
    docker compose -f example-development-docker-compose.yaml down -v

Port 8098 rather than 8080, so it stands beside the Server's own development
stack and `AlgoJudge-Runner`'s without either taking the other's port.

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
| `AJ_External__ApiBaseUrl` | where answers are read, when that is a different service |
| `AJ_External__Username`, `AJ_External__Password` | the robot account. **Secrets**, and they have no default |
| `AJ_External__UserId` | the judge's own numeric id for that account, resolved from the username when unset |
| `AJ_External__PollMinSeconds`, `PollMaxSeconds`, `PollEscalateAfterSeconds` | how often the judge is asked |
| `AJ_External__SubmitMinIntervalSeconds` | the gap between two submissions |
| `AJ_External__PendingTimeoutSeconds` | how long an unanswered submission is waited for |
| `AJ_External__MaxPending` | how many may be outstanding at once |

An unknown judge is refused at start-up, by name and with the list of what this
build knows. The addresses default to the default judge's own, so a deployment
of `uva` states neither — see [`docs/UVA.md`](docs/UVA.md).

**`AJ_Runner__ProblemTypes` may be left unset**, and usually is: silence declares
whatever the integration serves. Set it only to narrow or widen that
deliberately.

**`AJ_Runner__Tags` names the pools this Runner belongs to**, comma-separated.
The Server pairs a Runner with work when the two tag lists **share at least one**
entry, and an empty list on either side means `default` — so naming a pool takes
this Runner out of the general queue as surely as it puts it into a reserved one.

**It is a seed, not a setting.** The Server reads it at the **first**
registration and never again; from then on the operator owns it in the panel.
It exists so a room of machines is deployed from one file rather than tagged one
at a time, and it stops there: a Runner that could re-declare its tags on restart
would put itself into an examination's pool with nobody having approved it.
Changing the variable later changes nothing, deliberately.

Configuration is refused at start-up rather than discovered an hour later. These
are the refusals an operator meets:

- **`AJ_Lease__RequestSeconds` must exceed `AJ_External__PendingTimeoutSeconds`.**
  Otherwise the Server reclaims the job while this Runner is still waiting on the
  judge, and the next Runner to claim it submits the same solution again.
- **`AJ_External__PollMaxSeconds` must fit four times inside the lease.** A held
  lease is renewed on the polling cycle, so slowing the polling down to be polite
  to somebody else's service slows the renewing down with it — and a lease that
  expires between two renewals is the same double submission by another route.
- **`AJ_Lease__RequestSeconds` may not exceed 3600.** The Server clamps what it
  grants, so a larger request is a deadline of this Runner's own invention: it
  would renew against a lease it does not have and hold a job past the real one.
  **This bounds the variable the first bullet tells you to raise** — pushing
  `AJ_External__PendingTimeoutSeconds` up eventually leaves no legal lease above
  it, and the refusal says so.
- **`AJ_External__PollMaxSeconds` may not be below `PollMinSeconds`**, and
  `PollMinSeconds` may not be below twenty. An external judge may publish no rate
  limit at all, so that floor is not lowered.

## Running it end to end

The whole path, against a throwaway Server. **The last step sends a real
submission to a real service**, so it needs an account on that service and a
decision from whoever owns it. [`docs/UVA.md`](docs/UVA.md) walks the same five
steps through the integration that exists.

1. **A Server**, with external judging turned on. It ships **off**, and while it
   is off no external work is handed out at all.
2. **A problem** of an external problem type, with the version's `props` naming
   which problem it is at the judge. **Without it this Runner refuses the job
   before anything leaves**, and says so by name.
3. **An activity**, a round that has opened, the problem attached, somebody
   enrolled.
4. **This Runner**, pointed at that stack, then approved in the manager panel.
5. **Submit.** The verdict arrives on the polling interval and is reported to the
   Server as an ordinary result.

## Related repositories

- [AlgoJudge-Runner](https://github.com/AlgoJudge/AlgoJudge-Runner) — the
  sandboxing Runner, and the `aj-protocol` crate this one consumes over Git,
  pinned to a revision in `Cargo.toml`
- [AlgoJudge-Server](https://github.com/AlgoJudge/AlgoJudge-Server) — jobs,
  problems, results, and the switch that turns external judging on
- [AlgoJudge-Client](https://github.com/AlgoJudge/AlgoJudge-Client) — the web
  frontend, which renders an external result and names the judge behind it
- [AlgoJudge-Ops](https://github.com/AlgoJudge/AlgoJudge-Ops) — the production
  Compose stack
- [AlgoJudge-Docs](https://github.com/AlgoJudge/AlgoJudge-Docs) — the public
  documentation site, whose `/runner/` section covers routing and external
  judging

## Contributing

Open an issue saying what you expected, what happened, and how to reproduce it.
Or open a pull request against `main`: one subject per pull request, with a note
on what changes and why.

By contributing you agree that your work is licensed under the terms below.

## License

This project is licensed under the MIT License.
See [LICENSE](LICENSE).

Authors are listed in [AUTHORS.txt](AUTHORS.txt).
