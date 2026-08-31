//! The loop itself, against a Server that is not there.
//!
//! **Everything else in `tests/` stands in for the archive; this stands in for
//! the Server.** That is the gap this file closes. `run.rs` has no unit tests —
//! it cannot have, being an async loop over two HTTP clients — and the only
//! tests that drove it needed a real Server, so they are `#[ignore]`d and CI has
//! never run them. Four decisions the loop makes were therefore unreachable from
//! the gate, and three of them were wrong.
//!
//! It is fast rather than `#[ignore]`d on purpose: a lease of sixty seconds
//! against a poll ceiling of sixty makes `lease::ceiling` zero, so the first
//! failed renewal is already the last one, and the first judging cycle fires
//! immediately. The whole file is under a second.

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

const SOURCE: &str = "int main(){}\n";
const SID: i64 = 31254724;
const PID: i64 = 36;

/// The sha256 of `SOURCE`, which the cache verifies before the loop reads it.
fn source_sha256() -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(SOURCE.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// An archive that accepts a submission and names it.
async fn archive(server: &MockServer) {
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
            ResponseTemplate::new(302).insert_header(
                "location",
                format!(
                    "/index.php?option=com_onlinejudge&Itemid=25&page=submit_problem\
                 &category=&mosmsg=Submission+received+with+ID+{SID}"
                )
                .as_str(),
            ),
        )
        .mount(server)
        .await;
    // Wherever the redirect lands has to answer something.
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(server)
        .await;
}

/// uHunt, answering the catalogue and one row with `verdict`.
///
/// `0` is "in queue", which keeps the job outstanding — the state a lease has to
/// survive, and the one the give-up is decided in.
async fn uhunt(server: &MockServer, verdict: i64) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/p/num/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"pid":{PID},"num":100,"title":"The 3n + 1 problem","status":1,"rtl":3000}}"#
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/subs-user/.*$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"name":"A Robot","uname":"robot","subs":[[{SID},{PID},{verdict},0,1700000000,5,0]]}}"#
        )))
        .mount(server)
        .await;
}

/// One claimable job, and nothing after it.
///
/// `props` and `config` are the two documents the loop reads; `problemVersionProps`
/// is where the archive's problem number lives.
fn job(props: &str, config: &str, version_props: &str) -> String {
    format!(
        // `packageFileId` is the empty string rather than absent: an external
        // problem has no package, which is the whole reason this Runner exists.
        r#"{{"jobId":"job-1","submissionId":"sub-1","problemType":"uva@1",
             "attempt":1,"leaseToken":"token-1","leaseExpiresAt":"2026-08-31T12:00:00Z",
             "problemVersionId":"version-1","packageFileId":"","packageSha256":"",
             "props":{props},"config":{config},"problemVersionProps":{version_props},
             "files":[{{"name":"source","fileName":"main.c","fileId":"file-1","sha256":"{}","sizeBytes":13}}]}}"#,
        source_sha256()
    )
}

/// A Server that hands out one job and answers everything else.
///
/// `renew` is the parameter because it is what two of these tests turn.
async fn server_handing_out(mock: &MockServer, job_body: String, renew: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_string(job_body))
        .up_to_n_times(1)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/claim"))
        .respond_with(ResponseTemplate::new(204))
        .mount(mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/runner/files/file-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SOURCE))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/job-1/progress"))
        .respond_with(ResponseTemplate::new(204))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/job-1/lease"))
        .respond_with(renew)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/files"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"id":"attached-1","sha256":"00","sizeBytes":1,"mimeType":"application/json"}"#,
        ))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/job-1/files"))
        .respond_with(ResponseTemplate::new(204))
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/jobs/job-1/report"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"resultId":"result-1","state":"completed","duplicate":false}"#,
            ),
        )
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/runner/heartbeat"))
        .respond_with(ResponseTemplate::new(204))
        .mount(mock)
        .await;
}

/// A renewal the Server granted. **Every field `Lease` carries**: a short body
/// parses as an error, the loop reads that as the Server being unreachable, and
/// with a ceiling of zero the job is given up before `harvest` ever sees a
/// verdict — which is how the first draft of this file "passed" the give-up test
/// and failed the other three.
fn renewed() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        r#"{"jobId":"job-1","leaseToken":"token-1","leaseExpiresAt":"2026-08-31T12:00:00Z"}"#,
    )
}

/// `who` names the test, because each needs an identity and a cache of its own:
/// they run concurrently, and one path shared between them is two processes
/// writing one key file.
fn probe_config(
    who: &str,
    server: &str,
    site: &str,
    hunt: &str,
) -> algojudge_external_runner::config::Config {
    algojudge_external_runner::config::Config {
        server_base_url: format!("{server}/api/v1"),
        runner_name: "loop-probe".into(),
        problem_types: vec![],
        tags: vec![],
        key_path: std::env::temp_dir()
            .join(format!("loop-probe-{who}-{}.key", std::process::id()))
            .to_string_lossy()
            .into_owned(),
        cache_path: std::env::temp_dir()
            .join(format!("loop-probe-cache-{who}-{}", std::process::id()))
            .to_string_lossy()
            .into_owned(),
        cache_max_bytes: 64 * 1024 * 1024,
        // Sixty against sixty makes `lease::ceiling` zero, so one failed renewal
        // is already too many. `refuse_what_cannot_work` would refuse this, and
        // is deliberately not called: the point is to reach a decision that
        // takes twenty minutes under any configuration the product allows.
        lease_seconds: 60,
        external: algojudge_external_runner::config::External {
            judge: "uva".into(),
            base_url: format!("{site}/"),
            api_base_url: format!("{hunt}/"),
            username: "robot".into(),
            password: "not-a-real-password".into(),
            user_id: Some(1),
            poll_min: 20,
            poll_max: 60,
            poll_escalate_after: 120,
            submit_min_interval: 0,
            pending_timeout: 900,
            max_pending: 4,
            long_poll_enabled: false,
        },
    }
}

fn judge(
    config: &algojudge_external_runner::config::Config,
) -> algojudge_external_runner::uva::Uva {
    let external = &config.external;
    algojudge_external_runner::uva::Uva::new(
        algojudge_external_runner::uva::site::Site::new(
            external.base_url.clone(),
            external.username.clone(),
            external.password.clone(),
        )
        .expect("the client builds"),
        algojudge_external_runner::uva::uhunt::Uhunt::new(
            reqwest::Client::new(),
            external.api_base_url.clone(),
        ),
        external.username.clone(),
        external.user_id,
    )
}

/// Runs the loop until it has been quiet for a moment, then stops it.
///
/// `work` never returns, so it is spawned and aborted. Everything asserted on is
/// read off the mock afterwards.
async fn run_for(config: algojudge_external_runner::config::Config, how_long: Duration) {
    let identity = aj_protocol::Identity::load_or_create(&config.key_path).expect("an identity");
    let server = aj_protocol::Server::new(&config.server_base_url).expect("a Server");
    let cache = Arc::new(aj_protocol::Cache::new(
        std::path::PathBuf::from(&config.cache_path),
        config.cache_max_bytes,
    ));
    let judge = judge(&config);
    let mut runner = algojudge_external_runner::run::Runner::new(server, cache, judge, config);

    let working = tokio::spawn(async move { runner.work(&identity).await });
    tokio::time::sleep(how_long).await;
    working.abort();
    let _ = working.await;
}

fn posted_to(sent: &[wiremock::Request], suffix: &str) -> Vec<String> {
    sent.iter()
        .filter(|r| r.url.path().ends_with(suffix))
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

/// **A job given up on is reported, and was silently dropped.**
///
/// `Action::GiveUp` was implemented exactly as `DropSilently` — take the entry
/// out of the pending set, log, and say nothing — despite a contract reading
/// *"stop holding it and say why"*. The submission stays live on the archive,
/// this Runner forgets it, and the Server's reaper hands the job to somebody who
/// submits the same solution again.
///
/// The evidence has to arrive **before** the report: the Server accepts an
/// attachment only while the job is running, and reporting ends that.
#[tokio::test]
async fn a_job_given_up_on_is_reported_and_not_dropped() {
    let mock = MockServer::start().await;
    let site = MockServer::start().await;
    let hunt = MockServer::start().await;
    archive(&site).await;
    uhunt(&hunt, 0).await;
    server_handing_out(
        &mock,
        job(
            r#"{"language":"c89-gcc"}"#,
            r#"{"languages":[]}"#,
            r#"{"uva":{"problemNumber":100}}"#,
        ),
        // Unreachable, for ever.
        ResponseTemplate::new(503),
    )
    .await;

    run_for(
        probe_config("given-up", &mock.uri(), &site.uri(), &hunt.uri()),
        Duration::from_millis(1500),
    )
    .await;

    let sent = mock.received_requests().await.unwrap();
    let reports = posted_to(&sent, "/report");
    assert_eq!(reports.len(), 1, "the job was given up on in silence");
    assert!(
        reports[0].contains("\"infrastructureFailure\":true"),
        "{}",
        reports[0]
    );
    assert!(
        reports[0].contains("renewal cycles in a row"),
        "the report does not say why: {}",
        reports[0]
    );
    assert!(
        !posted_to(&sent, "/files").is_empty(),
        "the evidence never reached the Server",
    );
}

/// **A language the assignment excluded is a verdict, and reaches no archive.**
///
/// Two readers used to answer this question over the same two documents, and
/// `forward`'s copy — unreachable by construction — called it an infrastructure
/// failure where the other called it a verdict. This is the first test that can
/// enter that decision at all.
#[tokio::test]
async fn a_language_the_assignment_excluded_is_a_verdict_and_reaches_no_archive() {
    let mock = MockServer::start().await;
    let site = MockServer::start().await;
    let hunt = MockServer::start().await;
    archive(&site).await;
    uhunt(&hunt, 0).await;
    server_handing_out(
        &mock,
        job(
            r#"{"language":"c89-gcc"}"#,
            r#"{"languages":["java8"]}"#,
            r#"{"uva":{"problemNumber":100}}"#,
        ),
        ResponseTemplate::new(200).set_body_string(r#"{"leaseExpiresAt":"2026-08-31T12:00:00Z"}"#),
    )
    .await;

    run_for(
        probe_config("excluded", &mock.uri(), &site.uri(), &hunt.uri()),
        Duration::from_millis(800),
    )
    .await;

    let reports = posted_to(&mock.received_requests().await.unwrap(), "/report");
    assert_eq!(reports.len(), 1, "one answer, and it is a verdict");
    assert!(reports[0].contains("PolicyViolation"), "{}", reports[0]);
    assert!(
        reports[0].contains("\"infrastructureFailure\":false"),
        "a rule of the activity is not the machinery failing: {}",
        reports[0]
    );

    // **The assertion this test exists for.** Nothing the activity refuses may
    // reach somebody else's site.
    assert!(
        site.received_requests().await.unwrap().is_empty(),
        "the archive was contacted for a submission the activity refuses",
    );
}

/// **A failure says whether asking again could help.**
///
/// `permanent` chose a log level on this Runner's own stderr; the Server, which
/// is what a manager reads when they decide on a rejudge, was sent a reason that
/// was identical either way. uHunt verdict 15 is "can't be judged" — permanent —
/// and 10 is "submission error", which is not.
#[tokio::test]
async fn a_failure_says_whether_asking_again_could_help() {
    for (verdict, expected) in [(15, "will not help"), (10, "may help")] {
        let mock = MockServer::start().await;
        let site = MockServer::start().await;
        let hunt = MockServer::start().await;
        archive(&site).await;
        uhunt(&hunt, verdict).await;
        server_handing_out(
            &mock,
            job(
                r#"{"language":"c89-gcc"}"#,
                r#"{"languages":[]}"#,
                r#"{"uva":{"problemNumber":100}}"#,
            ),
            renewed(),
        )
        .await;

        run_for(
            probe_config("permanent", &mock.uri(), &site.uri(), &hunt.uri()),
            Duration::from_millis(1200),
        )
        .await;

        let reports = posted_to(&mock.received_requests().await.unwrap(), "/report");
        assert_eq!(reports.len(), 1, "verdict {verdict}");
        assert!(
            reports[0].contains(expected),
            "verdict {verdict} should say {expected:?}: {}",
            reports[0]
        );
    }
}

/// **An unreadable configuration fails once, naming the field.**
///
/// Two readers meant the message could have come from either; the one that
/// swallowed the error did so *because* the other owned the wording. After the
/// collapse the wording has to be the one that still arrives.
#[tokio::test]
async fn an_unreadable_problem_version_fails_once_with_the_message_that_names_the_field() {
    let mock = MockServer::start().await;
    let site = MockServer::start().await;
    let hunt = MockServer::start().await;
    archive(&site).await;
    uhunt(&hunt, 0).await;
    server_handing_out(
        &mock,
        job(r#"{"language":"c89-gcc"}"#, r#"{"languages":[]}"#, "null"),
        ResponseTemplate::new(200).set_body_string(r#"{"leaseExpiresAt":"2026-08-31T12:00:00Z"}"#),
    )
    .await;

    run_for(
        probe_config("unreadable", &mock.uri(), &site.uri(), &hunt.uri()),
        Duration::from_millis(800),
    )
    .await;

    let reports = posted_to(&mock.received_requests().await.unwrap(), "/report");
    assert_eq!(reports.len(), 1, "one failure, not two");
    assert!(
        reports[0].contains("\"infrastructureFailure\":true"),
        "{}",
        reports[0]
    );
    assert!(
        reports[0].contains("props"),
        "the message does not name the missing field: {}",
        reports[0]
    );
    assert!(
        site.received_requests().await.unwrap().is_empty(),
        "a job that cannot be read must reach no archive",
    );
}
