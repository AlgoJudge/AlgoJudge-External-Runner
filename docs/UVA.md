# UVa Online Judge

The one integration this Runner has. It serves the problem type **`uva@1`**
against [onlinejudge.org](https://onlinejudge.org), and it is `src/uva/`.

Everything here is specific to that archive. What the Runner does whatever it
forwards to is in [`../README.md`](../README.md).

## Two services, one judge

Submitting is an HTML form flow behind a session cookie on `onlinejudge.org`;
reading a verdict is a JSON API on `uhunt.onlinejudge.org`. That is why the
configuration has both a `BaseUrl` and an `ApiBaseUrl`, and why `src/uva/` has
`site.rs` beside `uhunt.rs`.

## Configuration

`AJ_External__Judge=uva` is the default, so a deployment forwarding to this
archive states none of the three addresses below — they are settings rather than
constants only so that an archive which moves does not need a release.

| Variable | Default here |
|---|---|
| `AJ_External__Judge` | `uva` |
| `AJ_External__BaseUrl` | `https://onlinejudge.org/` |
| `AJ_External__ApiBaseUrl` | `https://uhunt.onlinejudge.org/` |
| `AJ_External__UserId` | resolved from the username on the first submission |

**`AJ_External__UserId` is worth setting once you know it**, and the failure it
avoids is a quiet one. uHunt answers `0` for a name it has never seen, and `0`
parses — so a typo in `AJ_External__Username` used to resolve to uid 0, get
cached, and make every poll ask `subs-user/0/…`, which answers no rows. Nothing
matched, nothing was reported, and every submission aged out at
`AJ_External__PendingTimeoutSeconds` as an infrastructure failure while the
solutions sat judged on the real account. The lookup now refuses uid 0 by name.

**`AJ_External__PollMinSeconds` cannot go below twenty**, and the Runner refuses
to start if it does. onlinejudge.org publishes no rate limit at all — searched
2026-08-13, nothing found — and a politeness limit a deployment can silently
disable is not a limit.

## The six languages

What onlinejudge.org offers, and what to call each of them here. **The problem
type defines this list**, because it belongs to the archive rather than to any
one problem: it is `src/uva/language.rs`, and this table is that file written
out.

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

## What a `uva@1` problem carries

There is no package. The whole of the configuration is documents the Server
already carries and does not read:

- **the version's `props`** says *which problem this is* — the archive's number.
  Identity rather than settings, written once at import and inherited by every
  assignment. Without it the Runner refuses the job **before anything leaves**,
  and says so by name.
- **the assignment's `config`** says *how this course judges it* — which of the
  archive's verdicts count as solved, and which of the six languages are allowed.

There is no language map to write: `uva@1` defines its own six.

## A worked example, end to end

The generic sequence is in [`../README.md`](../README.md); this is that sequence
with the archive filled in. **The last step sends a real submission to a real
service**, so it needs a robot account and a decision from whoever owns it.

1. **A Server**, with external judging turned on — it ships **off**, and while it
   is off no external work is handed out at all.

2. **A problem.** Fetch the statement through the Server, because the archive
   sends no `Access-Control-Allow-Origin` and a browser cannot read it:

       POST /files/fetch   {"url": "https://onlinejudge.org/external/1/100.pdf"}
       POST /problems      {"slug":"UVa-100", "type":"uva@1", "external":true}
       POST /problems/{id}/versions
           {"statements":[…], "props":{"type":"uva@1","uva":{"problemNumber":100}}}

3. **An activity**, a round that has opened, the problem attached, somebody
   enrolled. **Attach after the configuration is right**: the assignment pins the
   problem version at the moment it is attached, deliberately, so publishing a
   correction afterwards does not change what a running round is judged against.

4. **This Runner**, pointed at that stack, then approved in the manager panel:

       AJ_Server__BaseUrl=http://host.docker.internal:8098/api/v1 \
       RUST_LOG=info ./x run --release

5. **Submit**, and watch:

       INFO  handed over  job=… judge=onlinejudge.org sid=31255986
       INFO  resolved the archive account  uid=…

   The verdict arrives on the polling interval and is reported to the Server as
   an ordinary result.

## The account

Every submission is made under one robot account, and **every real submission
stays on that account for ever**. That is a decision for whoever owns it.

When testing, use a solution written to be wrong: it keeps the account's solved
count honest, and it avoids the question of what a duplicated *accepted*
solution does, which nobody has measured.
