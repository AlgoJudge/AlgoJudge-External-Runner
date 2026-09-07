# Documentation

[`../README.md`](../README.md) is the Runner itself: what it does whatever it
forwards to, how to build and run it, and the variables an operator sets. The
file that carries **every** variable, with its default and the reasoning behind
it, is [`../.env.example`](../.env.example); a test compares its keys against the
source in both directions, so it is the one that cannot quietly fall behind.

**One file per integration lives here**, because what an archive offers — its
languages, its addresses, what a problem of its type carries — belongs to that
archive rather than to the Runner. A second integration is a second file beside
this one.

| | |
|---|---|
| [UVA.md](UVA.md) | **UVa Online Judge**, serving `uva@1` against `onlinejudge.org`. The only integration there is |

One file here is not an integration:

| | |
|---|---|
| [RELEASE.md](RELEASE.md) | what to do before pushing a `v*` tag, and where the version is written |
