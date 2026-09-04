//! What the Runner actually asks for when it takes a job.
//!
//! **Written because it did not ask for what it was configured to.** The lease
//! test held a job for a hundred and fifty seconds on what its configuration
//! called an eighty-second lease, and passed with lease renewal deleted — which
//! it should not have. The Server's own table said why: the job was granted
//! **six hundred** seconds, the Server's default, so nothing was ever close to
//! expiring and there was nothing for renewal to save.
//!
//! Everything about that is invisible from either side alone. The Runner logs
//! the lease it wants, the Server logs the lease it gave, and nobody compares
//! them. So this asserts on the bytes.
//!
//! Offline: the Server here is a stand-in, and this runs in an ordinary
//! `./x test`.

mod stack;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The claim carries the lease the Runner was configured with.
#[tokio::test]
async fn a_claim_asks_for_the_lease_it_was_configured_with() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/runner/jobs/claim"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = aj_protocol::Server::new(&server.uri()).expect("a Server");
    let taken = client.claim(Some(80), None).await.expect("claiming");
    assert!(taken.is_none(), "204 means nothing matched");

    let sent = server
        .received_requests()
        .await
        .expect("the stand-in records what it was sent");
    let body = String::from_utf8_lossy(&sent[0].body).into_owned();

    assert!(
        body.contains("\"leaseSeconds\":80"),
        "the claim asked for no particular lease, so the Server applies its own \
         default of ten minutes and a shorter one is a fiction: {body}"
    );
}

/// And the Server grants what was asked for.
///
/// **The half that cannot be checked offline, and the half that was wrong.** The
/// bytes above are right; the lease in the database was six hundred seconds all
/// the same, measured on 2026-08-22 across four runs. Every guard this Runner
/// has around leases — `refuse_what_cannot_work` refusing a `lease_seconds` that
/// does not exceed `pending_timeout`, and refusing a poll interval that does not
/// fit four times inside it — computes with a number that never reached the far
/// end. Two repositories each behaving correctly on their own.
#[tokio::test]
#[ignore = "needs a development Server; set AJ_TEST_SERVER"]
async fn the_server_grants_the_lease_that_was_asked_for() {
    stack::logs();
    stack::heartbeat();
    let admin = stack::Session::admin().await;
    let ready = stack::a_problem_to_submit_to(&admin, 100).await;
    stack::submit(
        &admin,
        &ready,
        "#include <cstdio>
int main(){return 0;}
",
    )
    .await;

    let key = std::env::temp_dir().join(format!("claim-lease-{}.key", std::process::id()));
    let identity =
        aj_protocol::Identity::load_or_create(key.to_string_lossy().as_ref()).expect("identity");
    let server = aj_protocol::Server::new(&stack::api()).expect("a Server");

    server
        .register(
            &aj_protocol::wire::Register {
                name: "claim-lease".into(),
                product: algojudge_external_runner::run::PRODUCT.into(),
                version: "0".into(),
                public_key: identity.public_key(),
                problem_types: vec!["uva@1".into()],
                tags: vec![],
                external: true,
                machine: None,
            },
            &identity,
        )
        .await
        .expect("registering");
    admin.approve_every_runner().await;
    server
        .authenticate(&identity)
        .await
        .expect("authenticating");

    let asked = 80u32;
    let job = loop {
        match server.claim(Some(asked), None).await.expect("claiming") {
            Some(job) => break job,
            None => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    };

    let granted = chrono_seconds_until(&job.lease_expires_at);
    assert!(
        granted < f64::from(asked) + 15.0,
        "asked for a {asked}s lease and was granted {granted:.0}s. The Server \
         applies its own default when it reads none, so every lease this Runner \
         computes with is a fiction and renewal is the only thing holding a job."
    );
}

/// Seconds from now to an RFC 3339 instant, without pulling in a date library.
///
/// **This shelled out to `date -u` until 2026-08-31**, and carried two dead
/// lines binding `SystemTime::UNIX_EPOCH` to a name it then discarded — the
/// remains of the version that did not. A test that spawns a process to read
/// the clock is a test that fails wherever that process is not on the path,
/// which in this repository is every host outside the toolchain container.
/// Unix time is UTC seconds with no leap seconds in it, so the remainder is
/// the same seconds-of-day the subprocess printed.
fn chrono_seconds_until(at: &str) -> f64 {
    // The Server answers RFC 3339 in UTC; only the distance matters, so this
    // reads the fields it needs rather than parsing a calendar.
    let seconds = |t: &str| -> f64 {
        let time = t.split('T').nth(1).unwrap_or("");
        let mut parts = time.trim_end_matches('Z').split(':');
        let h: f64 = parts.next().unwrap_or("0").parse().unwrap_or(0.0);
        let m: f64 = parts.next().unwrap_or("0").parse().unwrap_or(0.0);
        let s: f64 = parts.next().unwrap_or("0").parse().unwrap_or(0.0);
        h * 3600.0 + m * 60.0 + s
    };
    let now = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_secs()
        % 86_400) as f64;

    let mut delta = seconds(at) - now;
    if delta < 0.0 {
        delta += 86_400.0;
    }
    delta
}
