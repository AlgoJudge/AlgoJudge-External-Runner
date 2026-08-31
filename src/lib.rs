//! A Runner that judges nothing.
//!
//! It claims jobs of an external problem type through the ordinary Server–Runner
//! contract, forwards the source to the judging system that owns that type under
//! one account, waits for that system's verdict, and reports it back as an
//! ordinary result. **The Server learns nothing new**: it hands out a job whose
//! problem type it never parses and stores a verdict string it never reads.
//!
//! Nothing untrusted runs here, so there is no sandbox, no container runtime and
//! no cgroup preflight — the whole of `AlgoJudge-Runner`'s startup check is
//! irrelevant to a component that is an HTTP client with a timer.
//!
//! **One integration exists: `uva`**, UVa Online Judge. Everything the loop needs
//! from a judging system is declared in `integration`, and a second integration
//! is a module beside `uva` rather than a change to `run`.

pub mod config;
pub mod integration;
pub mod lease;
pub mod pending;
pub mod run;
pub mod schedule;
pub mod uva;
