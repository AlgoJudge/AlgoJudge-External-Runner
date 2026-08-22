//! A Server to run against, driven the way a person would.
//!
//! **This is the thing that was missing.** Four behaviours of this Runner's own
//! loop cannot be reached from `aj-protocol`'s conformance suite — renewal
//! firing, a lost lease dropped silently, an unreachable archive reported as
//! infrastructure, and submission staying serialised — and every one of them
//! needs a Server with a problem on it and somebody who has submitted.
//!
//! Everything here is a manager or a participant acting over HTTP: a session and
//! a cookie, never a Runner token. The Runner's own half of the conversation is
//! `aj-protocol`'s business and is tested there.
//!
//! ```text
//! AJ_TEST_SERVER=http://host.docker.internal:8098/api/v1 \
//!   ./x test --test stack -- --include-ignored
//! ```
//!
//! Nothing in here runs without `AJ_TEST_SERVER`; the whole file is `#[ignore]`d
//! so an ordinary `./x test` stays offline.

use serde_json::{json, Value};

pub fn api() -> String {
    std::env::var("AJ_TEST_SERVER").unwrap_or_else(|_| "http://localhost:8080/api/v1".into())
}

/// A name nothing else in the database holds.
///
/// **A millisecond is not unique enough**, and that is measured rather than
/// supposed: `lease.rs` declares `mod stack;`, so both tests are compiled into
/// one binary and libtest runs them at once — two calls landed in the same
/// millisecond and the second `POST /problems` came back **500**, a unique
/// index violation on `IX_Problems_Slug` reported as an internal error.
///
/// A counter fixes it inside one process; the clock still separates one run
/// from the next.
fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_millis();
    format!("{prefix}{now}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Somebody signed in, holding their cookie.
pub struct Session {
    http: reqwest::Client,
}

impl Session {
    pub async fn admin() -> Self {
        // **A timeout, and it is the whole reason this file could hang.** A
        // `reqwest` client has none by default, so one request that never
        // answers blocks for ever — which is what happened: the submit loop is
        // bounded at forty attempts and stood for over ten minutes inside one
        // of them, printing nothing, looking exactly like a deadlock.
        //
        // Thirty seconds is far longer than any call here takes and short
        // enough that a stall becomes a failure with a message rather than a
        // test somebody kills by hand.
        // **Three deadlines and no connection reuse**, which is more than a
        // client normally wants and exactly what a test client needs.
        //
        // Measured on 2026-08-22: with `.timeout(30s)` alone, the eighth
        // submission in a loop went out and never came back. The process was
        // alive, the print before the request had landed, no answer line
        // followed, and **the timeout did not fire**. Three probes taken during
        // that stall — `/instance` in 8 ms, `/health` in 4 ms, and the identical
        // multipart POST by `curl` in 30 ms — put the Server beyond suspicion.
        //
        // Seven requests had succeeded before it, so the first suspect was the
        // pooled keep-alive connection the eighth reused. `pool_max_idle_per_host(0)`
        // takes that variable away: every request opens its own connection.
        //
        // **It did not fix it, and that is worth recording.** With pooling off
        // the loop no longer froze, but each request took tens of seconds where
        // `curl` took thirty milliseconds. Measured afterwards: a container
        // reaching `host.docker.internal:8080` connects in **2 ms**, ten times
        // out of ten. So the transport is not slow — something between this
        // client and the socket is, and it is not any of: the Server, the
        // multipart shape, connection reuse, or the container's networking.
        // Each of those was excluded by its own measurement while a request was
        // stalled.
        //
        // The setting stays because slow-and-progressing beats frozen, and
        // because it is one fewer variable for whoever picks this up.
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(20))
            .pool_max_idle_per_host(0)
            .build()
            .expect("a client");

        let answer = http
            .post(format!("{}/identity/login?useSessionCookies=true", api()))
            .json(&json!({ "email": "admin", "password": "admin-development-only" }))
            .send()
            .await
            .expect("the Server is up — is a development stack running?");

        assert!(
            answer.status().is_success(),
            "signing in: {}",
            answer.status()
        );
        Self { http }
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let answer = self
            .http
            .post(format!("{}{path}", api()))
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {path}: {e}"));

        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        assert!(status.is_success(), "POST {path} answered {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    async fn put(&self, path: &str, body: Value) -> Value {
        let answer = self
            .http
            .put(format!("{}{path}", api()))
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("PUT {path}: {e}"));

        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        assert!(status.is_success(), "PUT {path} answered {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    pub async fn get(&self, path: &str) -> Value {
        let answer = self
            .http
            .get(format!("{}{path}", api()))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));

        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        assert!(status.is_success(), "GET {path} answered {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// **Both directions of the switch, because it governs both.** Off, no
    /// external work is handed out and nothing is fetched.
    pub async fn allow_external_judging(&self) {
        self.put(
            "/instance",
            json!({
                "localRegistrationEnabled": false,
                "requireEmail": false,
                "requireConfirmedEmail": false,
                "showLogo": true,
                "showLocalSignIn": true,
                "accountDeletionEnabled": true,
                "externalJudgingEnabled": true,
            }),
        )
        .await;
    }

    /// Approves every Runner waiting for it. Nothing is evaluated before this.
    pub async fn approve_every_runner(&self) {
        let listed = self.get("/runners").await;
        for runner in listed["items"].as_array().cloned().unwrap_or_default() {
            if runner["state"] == "approved" {
                continue;
            }
            let id = runner["id"].as_str().expect("a runner id");
            self.post(&format!("/runners/{id}/approve"), json!({}))
                .await;
        }
    }
}

/// A `uva@1` problem, attached to an open round, ready to be submitted to.
pub struct Ready {
    pub activity: String,
}

/// Builds one, in the order that matters.
///
/// **The configuration goes in before the problem is attached.** An assignment
/// pins the problem version at the moment of attaching — deliberately, so a
/// correction does not change what a running round is judged against — so a
/// version published afterwards is not what a job will read. That cost an
/// end-to-end run on 2026-08-16 two attempts.
pub async fn a_problem_to_submit_to(admin: &Session, problem_number: i64) -> Ready {
    // **Each step says it happened.** This is a dozen round trips to a Server
    // and one of them reaches `onlinejudge.org`; when it stalls, a test that
    // printed nothing was indistinguishable from a hang, and there was no way
    // to tell which step it had stalled on.
    let step = |what: &str| eprintln!("  · {what}");

    step("allowing external judging");
    admin.allow_external_judging().await;

    // The statement, fetched by the Server because the archive sends no
    // `Access-Control-Allow-Origin` and nothing else can read it.
    step("fetching the statement from onlinejudge.org");
    let statement = admin
        .post(
            "/files/fetch",
            json!({ "url": format!(
                "https://onlinejudge.org/external/{}/{problem_number}.pdf",
                problem_number / 100) }),
        )
        .await;
    let file = statement["id"].as_str().expect("a file id").to_owned();

    step("creating the problem");
    let problem = admin
        .post(
            "/problems",
            json!({
                "slug": unique("UVa-probe-"),
                "name": format!("UVa {problem_number}"),
                "type": "uva@1",
                "external": true,
            }),
        )
        .await;
    let problem_id = problem["id"].as_str().expect("a problem id").to_owned();

    step("publishing a version");
    admin
        .post(
            &format!("/problems/{problem_id}/versions"),
            json!({
                "statements": [{ "fileId": file }],
                // **`props`, not `config`, since 2026-08-22.** The number says
                // *which problem this is* — identity, which every assignment
                // inherits — and `config` is settings, of which only the
                // assignment's layer is left. Without it the Runner refuses the
                // job before anything leaves and says which field is missing.
                //
                // **No language map any more**: `uva@1` defines the archive's
                // six itself, because the list belongs to the archive and every
                // problem in it shares them.
                "props": {
                    "type": "uva@1",
                    "uva": { "problemNumber": problem_number },
                },
            }),
        )
        .await;

    step("creating the activity");
    let activity = unique("PROBE");
    admin
        .post(
            "/activities",
            json!({
                "slug": activity,
                "name": "UVa probe",
                "type": "contest@1",
                "rankingType": "icpc",
                "timeZone": "Europe/Warsaw",
                "joinPolicy": "open",
                // No `languages` here: an activity stopped carrying a list on
                // 2026-08-22. The allowed set is the assignment's, and for
                // `uva@1` the type's own six are the whole of it.
                "attachmentVisibility": [{ "name": "source", "visibility": "participant" }],
            }),
        )
        .await;

    step("opening a round");
    let round = admin
        .post(
            &format!("/activities/{activity}/series"),
            json!({
                "slug": "r1",
                "name": "Round 1",
                // Started already, so the scheduler opens it on its next scan
                // rather than this test deciding a round is open on its own.
                "startDate": "2020-01-01T00:00:00Z",
                "endDate": "2099-01-01T00:00:00Z",
            }),
        )
        .await;
    let round_id = round["id"].as_str().expect("a round id").to_owned();

    step("attaching the problem");
    admin
        .post(
            &format!("/series/{round_id}/problems"),
            json!({ "problemId": problem_id, "slug": "A", "maxPoints": 100 }),
        )
        .await;

    step("enrolling");
    admin
        .post(&format!("/activities/{activity}/enrolment"), json!({}))
        .await;

    Ready { activity }
}

/// Waits for the scheduler to open the round, then submits.
///
/// A round is opened by the scheduler's own scan rather than by the request that
/// created it, so a submission sent in that gap is a genuine 404 — waited out
/// rather than retried blindly.
pub async fn submit(admin: &Session, ready: &Ready, source: &str) -> String {
    let path = format!("/activities/{}/problems/A/submissions", ready.activity);

    // Eighty seconds at most, and it says so while it waits: a silent loop
    // here and a silent sleep in `lease.rs` were together three minutes of no
    // output, which reads exactly like a hang.
    //
    // **Each attempt announces itself before the request, not after.** A run on
    // 2026-08-22 stalled here for over ten minutes with the loop bounded at
    // forty attempts and a thirty-second client timeout in place — which is
    // impossible if the loop was turning, so the next run has to be able to say
    // whether the request was ever sent.
    for attempt in 0..40 {
        eprintln!("  submitting, attempt {} of 40", attempt + 1);

        // **One opaque document, and a file name.** The language was a field
        // the Server read; it is a member of `props` now, and the Server named
        // pasted source from a table of seven extensions it no longer has — so
        // the sender names it or the submission is refused.
        let form = reqwest::multipart::Form::new()
            .text("props", r#"{"type":"uva@1","language":"cpp11-gcc"}"#)
            .text("code", source.to_owned())
            .text("fileName", "main.cpp")
            .text("sha256", sha256_of(source));

        let answer = admin
            .http
            .post(format!("{}{path}", api()))
            .multipart(form)
            .send()
            .await
            .unwrap_or_else(|e| panic!("submitting: {e}"));

        let status = answer.status();
        if status.is_success() {
            let body: Value = answer.json().await.expect("a submission");
            return body["id"].as_str().expect("a submission id").to_owned();
        }
        eprintln!("    answered {status}");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    panic!("the round never opened, so nothing could be submitted");
}

fn sha256_of(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// **The smoke test for the harness itself**, and nothing more.
///
/// It proves a Server can be stood up to the point where this Runner would have
/// something to take: a `uva@1` problem, attached, open, with a submission
/// waiting. It deliberately does **not** start a Runner, so nothing reaches
/// `onlinejudge.org` — the four behaviours this harness exists for come next,
/// and the one that submits for real needs a decision from whoever owns the
/// account.
#[tokio::test]
#[ignore = "needs a development Server; set AJ_TEST_SERVER"]
async fn a_problem_can_be_stood_up_and_submitted_to() {
    let admin = Session::admin().await;
    let ready = a_problem_to_submit_to(&admin, 100).await;

    let submission = submit(
        &admin,
        &ready,
        "#include <cstdio>\nint main(){printf(\"deliberately wrong\\n\");return 0;}\n",
    )
    .await;

    let seen = admin
        .get(&format!(
            "/activities/{}/submissions/{submission}",
            ready.activity
        ))
        .await;

    // Queued and nothing more: no Runner has been started, so it waits.
    assert_eq!(seen["state"], "queued", "{seen}");
    admin.approve_every_runner().await;
}
