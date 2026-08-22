//! A Runner that judges nothing.
//!
//! It claims `uva@1` jobs through the ordinary Server–Runner contract, forwards
//! the source to `onlinejudge.org` under one account, waits for that archive's
//! verdict, and reports it back as an ordinary result. **The Server learns
//! nothing new**: it hands out a job whose problem type it never parses and
//! stores a verdict string it never reads.
//!
//! Nothing untrusted runs here, so there is no sandbox, no container runtime and
//! no cgroup preflight — the whole of `AlgoJudge-Runner`'s startup check is
//! irrelevant to a component that is an HTTP client with a timer.

pub mod config;
pub mod language;
pub mod lease;
pub mod pending;
pub mod problem;
pub mod report;
pub mod run;
pub mod schedule;
pub mod uva;
pub mod verdict;
