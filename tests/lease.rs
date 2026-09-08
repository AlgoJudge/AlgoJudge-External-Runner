//! Holding a job while somebody else's judge thinks about it.
//!
//! # It runs now, and it does not yet prove what it was written to prove
//!
//! Measured 2026-08-22 against a live development stack. Two faults were in the
//! test itself and are fixed; a third is in the Server and is not.
//!
//! **It used to hang for ever, and that was a deadlock written into it.**
//! `run::admitted` does not return until a manager approves the Runner, and the
//! approval was on the next line. Everything that looked like a network fault —
//! a socket idle with both queues empty, no deadline firing, every thread asleep
//! — was this loop waiting for a line it could never reach. A heartbeat task
//! settled it in one run: the runtime was ticking punctually the whole time, so
//! nothing was blocked, and one task was simply never going to be woken.
//!
//! Three more faults surfaced once it could run: the archive stand-in answered a
//! `location` without the phrase `sid_from` reads, `Uhunt` was given a base
//! already ending in `api/` so every lookup asked for `/api/api/…`, and
//! `pending_timeout` was sixty seconds — the Runner gave up on the archive
//! before the lease could matter.
//!
//! **The fifth was in the Server, and is fixed.** `ProgressAsync` extended a
//! held lease by `DefaultLease` — ten minutes — whatever the Runner asked for at
//! claim, and `Runner::take` reports progress the instant it takes a job. So the
//! eighty seconds configured here were granted, reported back as eighty, and
//! replaced by six hundred a fraction of a second later: the claim answer said
//! `22:09:52` and the row said `22:18:32`. Nothing on either side disagreed out
//! loud, and this test passed with `renew_everything()` deleted.
//!
//! An `EvaluationJob` now records the lease it was granted and a heartbeat
//! renews by that. Measured afterwards, on both sides of the change:
//!
//! - with renewal, the job is still `running` at 150s — **passes**;
//! - with `renew_everything()` deleted, it goes back to `queued` at **100s** —
//!   eighty seconds of lease and up to thirty of reaper cadence — and this test
//!   says so.
//!
//! **The pattern for all of this already existed.**
//! `a_renewed_lease_outlives_the_deadline_it_was_granted` in
//! `AlgoJudge-Runner`'s `crates/aj-runner/tests/end_to_end.rs` had solved the
//! approval deadlock correctly since 2026-08-16 — approving in a spawned task
//! beside `admitted`, with a comment saying that doing it afterwards would hang
//! the test rather than fail it. Copying it would have saved every measurement
//! above.
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
        // **The real redirect, not a sketch of one.** `site::sid_from` reads the
        // id out of the phrase `Submission received with ID`, and a `location`
        // without it parses as no id at all — which the client reports as a
        // lapsed session, because that is what an archive that took nothing
        // usually means. The shape is the one captured from the live archive on
        // 2026-08-16 and asserted in `site.rs`'s own unit test.
        .respond_with(
            ResponseTemplate::new(302).insert_header(
                "location",
                format!(
                    "/index.php?option=com_onlinejudge&Itemid=25&page=submit_problem\
                 &category=&mosmsg=Submission+received+with+ID+{sid}"
                )
                .as_str(),
            ),
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
/// A lease of eighty seconds, a job held past it, and the Server asked whether
/// it is still ours. Without renewal `LeaseReaper` takes it back within thirty
/// seconds of the deadline and the submission returns to the queue — where
/// another Runner claims it and sends the same solution to onlinejudge.org a
/// second time.
///
/// # Why the configuration is one the product refuses
///
/// `Config::refuse_what_cannot_work` requires `lease_seconds > pending_timeout`,
/// and this test sets 80 against 300. That is deliberate, and it is the only way
/// to run this in three minutes.
///
/// The Server grants what is asked for, clamped to `[60, 3600]` — sixty is the
/// **floor**, not the ceiling, which this file claimed the other way round until
/// it was read. So under a configuration that passes validation the Runner
/// always gives up on the archive before the lease it holds could expire, and
/// renewal never has to save anything.
///
/// Except at the top: `lease_seconds` of 3700 against a `pending_timeout` of
/// 3650 passes validation, the Server grants 3600, and the job is then held
/// fifty seconds past the lease. **That is reachable by configuration and
/// renewal is the only thing standing in front of it** — and it takes an hour
/// to reach. Eighty against three hundred is the same shape, in ninety seconds.
///
/// Building `Config` here rather than through `from_environment` is what makes
/// that possible; an operator cannot do it.
// **Two worker threads, and that is a finding rather than a preference.**
//
// `#[tokio::test]` defaults to a single-threaded runtime. Every deadline in
// this file — `timeout`, `connect_timeout`, `read_timeout` — is a tokio timer,
// and a timer on a single-threaded runtime cannot fire while that one thread is
// blocked. Measured on 2026-08-22: around the seventh request the loop froze
// with **none of the three firing**, while the Server answered three separate
// probes in single-digit milliseconds. A stalled network call would have been
// cut by a deadline; a blocked thread is what explains all three failing at
// once.
//
// A second worker leaves the timer thread free, so a blocked call becomes a
// timeout with a message instead of a silence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a development Server; set AJ_TEST_SERVER. Takes about three minutes."]
async fn a_held_job_outlives_the_lease_it_was_granted() {
    stack::logs();
    stack::heartbeat();
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
    config.external.pending_timeout = 300;
    config.external.poll_min = 20;
    config.external.poll_max = 20;
    config.external.poll_escalate_after = 20;

    let identity = aj_protocol::Identity::load_or_create(&config.key_path).expect("an identity");
    let server = aj_protocol::Server::new(&config.server_base_url).expect("a Server");

    // **The approval runs beside admission, not after it**, and getting that
    // wrong is what made this test look like a network fault for a day.
    //
    // `run::admitted` does not return until a manager has approved this Runner:
    // it registers, is told `pendingApproval`, waits, and registers again, for
    // ever. Nobody but this test is going to approve it. The version deleted
    // here awaited admission on one line and approved on the next — a line that
    // could never be reached — so the test sat silently re-registering until
    // somebody killed it. Ten `lease-probe` rows were left `pendingApproval` on
    // the development Server proving exactly that.
    //
    // Diagnosing it took a heartbeat: the process was quiet, and quiet has two
    // causes — a dead runtime, or a live one with a task that will never wake.
    // A line every five seconds told them apart in one run.
    let approving = tokio::spawn({
        let admin = admin.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                admin.approve_every_runner().await;
            }
        }
    });
    // The judge is built before admission because registration declares what it
    // serves: an empty `AJ_Runner__ProblemTypes` is the judge's own type.
    let judge = probe_judge(&site.uri(), &hunt.uri());
    let (never, _teller) = aj_protocol::stopping::Stopping::told();
    algojudge_external_runner::run::admitted(&server, &identity, &config, &judge, &never)
        .await
        .expect("being admitted");
    approving.abort();
    let _ = approving.await;

    // From the configuration, like the binary does, rather than beside it: two
    // places naming one directory is how the image came to ship without it.
    // The ceiling was doing exactly that one line down until 2026-08-31.
    let cache = Arc::new(aj_protocol::Cache::new(
        std::path::PathBuf::from(&config.cache_path),
        config.cache_max_bytes,
        identity.fingerprint(),
    ));
    let mut runner =
        algojudge_external_runner::run::Runner::new(Arc::new(server), cache, judge, config);

    let working = tokio::spawn(async move {
        // Reported rather than swallowed: this returning at all is a fault, and
        // the loop is the only thing that knows why.
        // Never told to stop: this test ends by aborting the task, and what a
        // stopped Runner does is `loop.rs`'s business.
        let (stopping, _teller) = aj_protocol::stopping::Stopping::told();
        if let Err(e) = runner.work(&identity, &stopping).await {
            tracing::error!(%e, "the Runner's loop gave up");
        }
    });

    // **Two phases, because the clock has to start when the job is taken.**
    //
    // This was one loop that failed the moment it saw `queued`, and `queued` is
    // what a submission *is* until a Runner claims it — so it failed at zero
    // seconds, every time, on the state it was created in. The archive is polled
    // every twenty seconds, so claiming is not instant.
    //
    // Waiting first also makes the measurement honest: a hundred and fifty
    // seconds counted from the loop's start would be a hundred and fifty minus
    // however long claiming took, which can fall under the eighty-second lease
    // and prove nothing at all.
    let path = format!("/activities/{}/submissions/{submission}", ready.activity);
    let state_of = |seen: &serde_json::Value| seen["state"].as_str().unwrap_or("?").to_owned();

    let mut last = String::new();
    let say = |elapsed: u64, state: &str, last: &mut String| {
        if state != last {
            eprintln!("  {elapsed:>3}s  {state}");
            *last = state.to_owned();
        }
    };

    // Phase one: until this Runner holds it.
    let waiting = std::time::Instant::now();
    loop {
        let seen = admin.get(&path).await;
        let state = state_of(&seen);
        say(waiting.elapsed().as_secs(), &state, &mut last);

        if state == "running" {
            break;
        }
        // **A terminal state ends this now rather than in ninety seconds.** A
        // job that was claimed and settled is a different failure from one
        // nobody took, and waiting out the clock to say so hides which happened.
        assert!(
            state != "failed" && state != "finished",
            "the job settled as {state} instead of being held, so there is no lease to outlive: {seen}"
        );
        assert!(
            waiting.elapsed() < Duration::from_secs(90),
            "no Runner claimed the job in 90s, so there is no lease to outlive: {seen}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Phase two: held past the deadline it was granted.
    //
    // A hundred and fifty rather than a hundred and twenty: the reaper runs on
    // its own cadence, so an eighty-second lease is reclaimed somewhere up to
    // thirty seconds after it lapses, and a window that ends at a hundred and
    // twenty leaves ten seconds of margin — thin enough to pass a build that
    // should have failed.
    eprintln!("  held — watching for 150s, on an 80s lease");
    let holding = std::time::Instant::now();
    let mut seen = admin.get(&path).await;

    while holding.elapsed() < Duration::from_secs(150) {
        seen = admin.get(&path).await;
        let state = state_of(&seen);
        say(holding.elapsed().as_secs(), &state, &mut last);

        // **The failure this test exists to catch, the moment it happens.** A
        // lease that expired is a job back in the queue: the Server hands it to
        // whoever asks next, and this Runner is still holding the archive's side
        // of the same submission.
        if state == "queued" {
            working.abort();
            let _ = working.await;
            panic!(
                "the job was taken back after {}s of holding, while this Runner was still holding it: {seen}",
                holding.elapsed().as_secs(),
            );
        }

        // **Settling is not this test's business, and waiting it out hides
        // why.** The stand-in never answers a verdict, so a job that reaches
        // `failed` or `finished` says the Runner gave up on it — for a reason
        // its log has just printed. Two minutes of polling afterwards adds
        // nothing but two minutes.
        assert!(
            state != "failed" && state != "finished",
            "the job settled as {state} after {}s instead of being held — \
             the Runner's log above says why: {seen}",
            holding.elapsed().as_secs(),
        );

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    assert_eq!(
        seen["state"], "running",
        "the job was taken back while this Runner was still holding it: {seen}"
    );
}

fn probe_config(site: &str, hunt: &str) -> algojudge_external_runner::config::Config {
    algojudge_external_runner::config::Config {
        server_base_url: stack::api(),
        runner_name: "lease-probe".into(),
        // Empty is the judge's own type, which is what a deployment gets.
        problem_types: vec![],
        tags: vec![],
        key_path: std::env::temp_dir()
            .join(format!("lease-probe-{}.key", std::process::id()))
            .to_string_lossy()
            .into_owned(),
        cache_path: std::env::temp_dir()
            .join("lease-probe-cache")
            .to_string_lossy()
            .into_owned(),
        cache_max_bytes: 64 * 1024 * 1024,
        lease_seconds: 80,
        poll_wait: 0,
        claim_poll_min: 1,
        claim_poll_max: 30,
        external: algojudge_external_runner::config::External {
            judge: "uva".into(),
            base_url: format!("{site}/"),
            api_base_url: format!("{hunt}/"),
            username: "robot".into(),
            password: "not-a-real-password".into(),
            user_id: Some(1),
            poll_min: 20,
            poll_max: 20,
            poll_escalate_after: 20,
            submit_min_interval: 1,
            pending_timeout: 300,
            max_pending: 20,
            long_poll_enabled: false,
        },
    }
}

/// The judge the probe drives, pointed at the two stand-ins.
///
/// **The origin, with no `api/`.** `Uhunt::text` appends that itself, so a base
/// already carrying it asks for `/api/api/p/num/100`, which the stand-in does not
/// serve — and a 404 from uHunt is reported as an infrastructure failure, so the
/// job settled in twenty-one milliseconds instead of being held. `probe_config`
/// had it right and the construction beside it did not.
fn probe_judge(site: &str, hunt: &str) -> algojudge_external_runner::uva::Uva {
    algojudge_external_runner::uva::Uva::new(
        algojudge_external_runner::uva::site::Site::new(
            format!("{site}/"),
            "robot".into(),
            "not-a-real-password".into(),
        )
        .expect("a site client"),
        algojudge_external_runner::uva::uhunt::Uhunt::new(
            reqwest::Client::new(),
            format!("{hunt}/"),
        ),
        "robot".into(),
        // Given rather than resolved, so the stand-in needs no account lookup.
        Some(1),
    )
}
