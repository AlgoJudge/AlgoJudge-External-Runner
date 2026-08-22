//! Holding a job while somebody else's judge thinks about it.
//!
//! **The behaviour with no output.** A lease being renewed looks exactly like
//! one that has not expired yet, so the only way to see it is to hold a job past
//! the deadline the Server granted and then ask the Server whose it is.
//!
//! Nothing here reaches `onlinejudge.org`: the archive and uHunt are stand-ins
//! started in process, and the only real thing on the far end is a Server. What
//! is under test is this Runner's own loop, which `aj-protocol`'s conformance
//! suite cannot see.
//!
//! ```text
//! AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1 \
//!   ./x test --test lease -- --include-ignored --nocapture
//! ```

mod stack;

use std::sync::Arc;
use std::time::Duration;

use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LOGIN_PAGE: &str = r#"<html><body>
  <form id="mod_loginform" action="/index.php" method="post">
    <input type="hidden" name="option" value="com_comprofiler">
    <input type="hidden" name="7f1b1a2c3d4e5f60718293a4b5c6d7e8" value="1">
  </form>
</body></html>"#;

/// An archive that accepts a submission and gives it an id.
async fn archive(server: &MockServer, sid: i64) {
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOGIN_PAGE))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/index.php"))
        .and(query_param("task", "login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>logout</html>"))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/index.php"))
        .and(query_param("page", "save_submission"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("/index.php?...+{sid}+...").as_str()),
        )
        .mount(server)
        .await;
}

/// A catalogue that answers, and a submission list that **never settles**.
///
/// Verdict `0` is "in queue", so the Runner keeps the job and keeps waiting —
/// which is the situation a lease has to survive.
async fn uhunt_still_thinking(server: &MockServer, sid: i64, pid: i64) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/p/num/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"pid":{pid},"num":100,"title":"The 3n + 1 problem","status":1,"rtl":3000}}"#
        )))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/api/subs-user/.*$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"name":"A Robot","uname":"robot","subs":[[{sid},{pid},0,0,1700000000,5,0]]}}"#
        )))
        .mount(server)
        .await;
}

/// <b>The one that matters.</b>
///
/// A lease of eighty seconds, a job held for a hundred and twenty, and the
/// Server asked afterwards whether it is still ours. Without renewal
/// `LeaseReaper` takes it back within thirty seconds of the deadline and the
/// submission returns to the queue.
///
/// The numbers are the smallest the configuration permits: the poll floor is
/// twenty seconds — it is somebody else's service — and the poll interval has to
/// fit four times inside the lease, so eighty is the floor for the lease too.
#[tokio::test]
#[ignore = "needs a development Server; set AJ_TEST_SERVER. Takes about three minutes."]
async fn a_held_job_outlives_the_lease_it_was_granted() {
    let admin = stack::Session::admin().await;
    let ready = stack::a_problem_to_submit_to(&admin, 100).await;
    let submission =
        stack::submit(&admin, &ready, "#include <cstdio>\nint main(){return 0;}\n").await;

    let site = MockServer::start().await;
    let hunt = MockServer::start().await;
    archive(&site, 31_000_001).await;
    uhunt_still_thinking(&hunt, 31_000_001, 36).await;

    let mut config = probe_config(&site.uri(), &hunt.uri());
    config.lease_seconds = 80;
    config.poll_min = 20;
    config.poll_max = 20;
    config.poll_escalate_after = 20;

    let identity = aj_protocol::Identity::load_or_create(&config.key_path).expect("an identity");
    let server = aj_protocol::Server::new(&config.server_base_url).expect("a Server");
    algojudge_runner_uva::run::admitted(&server, &identity, &config)
        .await
        .expect("registering");
    admin.approve_every_runner().await;
    algojudge_runner_uva::run::admitted(&server, &identity, &config)
        .await
        .expect("being admitted");

    let cache = Arc::new(aj_protocol::Cache::new(
        std::env::temp_dir().join("lease-probe-cache"),
        64 * 1024 * 1024,
    ));
    let mut runner = algojudge_runner_uva::run::Runner::new(
        server,
        cache,
        algojudge_runner_uva::uva::site::Site::new(
            format!("{}/", site.uri()),
            "robot".into(),
            "not-a-real-password".into(),
        )
        .expect("a site client"),
        algojudge_runner_uva::uva::uhunt::Uhunt::new(
            reqwest::Client::new(),
            format!("{}/api/", hunt.uri()),
        ),
        config,
        // Given rather than resolved, so the stand-in needs no account lookup.
        Some(1),
    );

    let working = tokio::spawn(async move {
        let _ = runner.work(&identity).await;
    });

    // **Watched rather than slept through.** This was one `sleep(120)` and then
    // a single look, which had three faults: it printed nothing for two minutes
    // so an ordinary slow run was indistinguishable from a hang, it waited the
    // whole time even when the answer had already gone wrong, and a failure
    // said only what the state was at the end rather than when it changed.
    //
    // It cannot be made quick. The floor is the Server's own: a lease is
    // clamped to sixty seconds and the poll interval has to fit four times
    // inside it, so eighty is the shortest lease this can be run with, and the
    // evidence is the job still being held *after* it. What it can be is
    // legible while it waits and immediate when it fails.
    let path = format!("/activities/{}/submissions/{submission}", ready.activity);
    let deadline = Duration::from_secs(120);
    let started = std::time::Instant::now();
    let mut last = String::new();

    while started.elapsed() < deadline {
        let seen = admin.get(&path).await;
        let state = seen["state"].as_str().unwrap_or("?").to_owned();

        if state != last {
            eprintln!("  {:>3}s  {state}", started.elapsed().as_secs());
            last = state.clone();
        }

        // **The failure this test exists to catch, the moment it happens.** A
        // lease that expired is a job back in the queue: the Server hands it to
        // whoever asks next, and this Runner is still holding the archive's
        // side of the same submission.
        if state == "queued" {
            working.abort();
            let _ = working.await;
            panic!(
                "the job was taken back after {}s, while this Runner was still                  holding it: {seen}",
                started.elapsed().as_secs(),
            );
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let seen = admin.get(&path).await;

    // Reaped rather than only cancelled: `abort` marks the task, and awaiting it
    // is what makes sure it has stopped before the stand-ins it is talking to
    // are dropped at the end of this function.
    working.abort();
    let _ = working.await;

    assert_eq!(
        seen["state"], "running",
        "the job was taken back while this Runner was still holding it: {seen}"
    );
}

fn probe_config(site: &str, hunt: &str) -> algojudge_runner_uva::config::Config {
    algojudge_runner_uva::config::Config {
        server_base_url: stack::api(),
        runner_name: "lease-probe".into(),
        problem_types: vec!["uva@1".into()],
        key_path: std::env::temp_dir()
            .join(format!("lease-probe-{}.key", std::process::id()))
            .to_string_lossy()
            .into_owned(),
        uva_base_url: format!("{site}/"),
        uhunt_base_url: format!("{hunt}/"),
        uva_username: "robot".into(),
        uva_password: "not-a-real-password".into(),
        uva_user_id: Some(1),
        poll_min: 20,
        poll_max: 20,
        poll_escalate_after: 20,
        submit_min_interval: 1,
        pending_timeout: 60,
        max_pending: 20,
        long_poll_enabled: false,
        lease_seconds: 80,
    }
}
