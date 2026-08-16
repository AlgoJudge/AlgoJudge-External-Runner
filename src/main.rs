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

mod config;
mod pending;
mod schedule;
mod uva;
mod verdict;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = config::Config::from_environment()?;
    tracing::info!(
        name = %config.runner_name,
        types = ?config.problem_types,
        "not yet claiming: the loop lands in the next commit"
    );
    Ok(())
}
