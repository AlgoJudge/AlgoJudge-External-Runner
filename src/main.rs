//! The binary: read the environment, pick the integration, and hand over to the
//! loop.
//!
//! **The choice of judging system is made exactly here.** `AJ_External__Judge`
//! names it, one arm builds it, and everything past this file is generic over
//! `integration::Judge` — so a second integration is a module and an arm.

use algojudge_external_runner::{config, integration::Judge, run, uva};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = config::Config::from_environment()?;

    match config.external.judge.as_str() {
        "uva" => start(uva_judge(&config)?, config).await,
        other => anyhow::bail!(
            "AJ_External__Judge is {other:?}; this build knows {:?}",
            [config::DEFAULT_JUDGE]
        ),
    }
}

/// UVa Online Judge, out of the external section of the configuration.
///
/// Nothing here reaches the archive: both clients are built, and the account is
/// not resolved until something has been submitted.
fn uva_judge(config: &config::Config) -> anyhow::Result<uva::Uva> {
    let external = &config.external;

    let http = reqwest::Client::builder()
        .user_agent(concat!(
            "AlgoJudge-External-Runner/",
            env!("CARGO_PKG_VERSION"),
            " (+https://algojudge.app)"
        ))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    Ok(uva::Uva::new(
        uva::site::Site::new(
            external.base_url.clone(),
            external.username.clone(),
            external.password.clone(),
        )?,
        uva::uhunt::Uhunt::new(http, external.api_base_url.clone()),
        external.username.clone(),
        external.user_id,
    ))
}

/// Greet both hosts, then run until something stops it.
async fn start<J: Judge>(judge: J, config: config::Config) -> anyhow::Result<()> {
    let identity = aj_protocol::Identity::load_or_create(&config.key_path)?;
    let server = aj_protocol::Server::new(&config.server_base_url)?;

    // The cache is the protocol crate's, used only to fetch a submission's
    // source with its checksum verified. No package is ever downloaded: an
    // external problem has none, and its whole configuration travels on the job.
    let cache = std::sync::Arc::new(aj_protocol::Cache::new(
        std::path::PathBuf::from("/var/cache/algojudge-external-runner"),
        256 * 1024 * 1024,
    ));

    // **What it will declare, not what was configured.** An empty
    // `AJ_Runner__ProblemTypes` is the judge's own type, and a start-up line
    // showing `[]` would send an operator looking for a setting that is working.
    let types = if config.problem_types.is_empty() {
        vec![judge.problem_type().to_owned()]
    } else {
        config.problem_types.clone()
    };
    tracing::info!(
        name = %config.runner_name,
        judge = %config.external.judge,
        types = ?types,
        tags = ?config.tags,
        "starting",
    );
    if config.external.long_poll_enabled {
        // Said out loud rather than left to be inferred from latency: the flag
        // is accepted, the accelerator behind it is not built, and the interval
        // net is doing the whole job.
        tracing::warn!(
            "AJ_External__LongPollEnabled is on, but the trigger is not built yet; \
             verdicts arrive on the interval net alone"
        );
    }

    // Nothing touches the judge before this point: a Runner that starts while
    // the judging system is down still registers and waits.
    run::admitted(&server, &identity, &config, &judge).await?;
    let mut runner = run::Runner::new(server, cache, judge, config);
    runner.work(&identity).await
}
