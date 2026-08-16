//! The binary: read the environment, greet both hosts, and hand over to the loop.

use algojudge_runner_uva::{config, run, uva};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = config::Config::from_environment()?;
    let identity = aj_protocol::Identity::load_or_create(&config.key_path)?;
    let server = aj_protocol::Server::new(&config.server_base_url)?;

    // The cache is the protocol crate's, used only to fetch a submission's
    // source with its checksum verified. No package is ever downloaded: a
    // `uva@1` problem has none, and its whole configuration travels on the job.
    let cache = std::sync::Arc::new(aj_protocol::Cache::new(
        std::path::PathBuf::from("/var/cache/algojudge-runner-uva"),
        256 * 1024 * 1024,
    ));

    let http = reqwest::Client::builder()
        .user_agent(concat!(
            "AlgoJudge-Runner-UVa/",
            env!("CARGO_PKG_VERSION"),
            " (+https://algojudge.app)"
        ))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let uhunt = uva::uhunt::Uhunt::new(http, config.uhunt_base_url.clone());
    let site = uva::site::Site::new(
        config.uva_base_url.clone(),
        config.uva_username.clone(),
        config.uva_password.clone(),
    )?;

    tracing::info!(name = %config.runner_name, types = ?config.problem_types, "starting");
    if config.long_poll_enabled {
        // Said out loud rather than left to be inferred from latency: the flag
        // is accepted, the accelerator behind it is not built, and the interval
        // net is doing the whole job.
        tracing::warn!(
            "AJ_Uva__LongPollEnabled is on, but the trigger is not built yet;              verdicts arrive on the interval net alone"
        );
    }

    // Nothing touches onlinejudge.org before this point: a Runner that starts
    // while the archive is down still registers and waits.
    let uva_user_id = config.uva_user_id;
    run::admitted(&server, &identity, &config).await?;
    let mut runner = run::Runner::new(server, cache, site, uhunt, config, uva_user_id);
    runner.work(&identity).await
}
