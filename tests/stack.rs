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

/// The Runner's own log, on stderr.
///
/// **Without this the Runner is mute.** It reports through `tracing`, and
/// `tracing` with no subscriber installed discards everything — so a job that
/// failed in twenty-one milliseconds said only "failed", and the one sentence
/// naming the reason was written and thrown away. `RUST_LOG` still overrides.
pub fn logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("algojudge_external_runner=debug,info")
    });
    // Not `init`: two tests in one binary would each try, and the second would
    // panic on an installed global.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .try_init();
}

/// A line every five seconds, for as long as the test runs.
///
/// **The discriminator the diagnosis was missing.** When a request stalls the
/// whole process goes quiet, and quiet has two very different causes: a runtime
/// that is dead, or a runtime that is fine with one task that will never be
/// woken. Nothing measured so far could tell them apart. This keeps ticking in
/// the second case and stops in the first.
pub fn heartbeat() {
    tokio::spawn(async {
        let started = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            eprintln!("    ♥ {}s", started.elapsed().as_secs());
        }
    });
}

/// Sends, and refuses to wait for ever.
///
/// **`reqwest`'s own three deadlines have never fired on the stall** — not
/// `timeout`, not `read_timeout`, not `connect_timeout` — so this asks tokio
/// directly. If this one fires, the runtime is alive and one request is wedged,
/// and the message says which. If it does not fire either, the timer wheel is
/// not turning and nothing built on it will save this test.
async fn send(builder: reqwest::RequestBuilder, what: &str) -> reqwest::Response {
    match tokio::time::timeout(std::time::Duration::from_secs(25), builder.send()).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => panic!("{what}: {e}"),
        Err(_) => panic!("{what}: no answer in 25s, and reqwest's own timeout never fired"),
    }
}

/// Somebody signed in, holding their cookie.
///
/// **It holds the cookie and not a client**, and that is an experiment rather
/// than a preference — see `client()`.
#[derive(Clone)]
pub struct Session {
    cookie: String,
}

impl Session {
    /// A client that has never sent anything before.
    ///
    /// **One client used to serve the whole session, and that is the last thing
    /// left unexcluded.** The stall recorded in `lease.rs` arrives after a dozen
    /// or so requests on one client — the third submit attempt in one run, the
    /// eighth in another, but the twelfth and the seventeenth counting from
    /// login, because the eight setup calls go through the same one. Seven
    /// candidates were excluded by measurement and not one of them touched the
    /// client object itself: `pool_max_idle_per_host(0)` took away reused
    /// connections and left the shared resolver, the shared connector and the
    /// cookie jar's lock exactly where they were.
    ///
    /// So: nothing shared at all. A client per request, and the session carried
    /// as a header rather than by a jar. If the stall goes, it lives in that
    /// state; if it stays, it is process-wide and a debugger is next.
    ///
    /// The three deadlines stay. None of them has ever fired on the stall — that
    /// is part of what makes it strange — but a run that fails with a message
    /// beats one somebody kills by hand.
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("a client")
    }

    /// A request already carrying the session, on a client of its own.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        Self::client()
            .request(method, format!("{}{path}", api()))
            .header(reqwest::header::COOKIE, &self.cookie)
    }

    pub async fn admin() -> Self {
        let answer = send(
            Self::client()
                .post(format!("{}/identity/login?useSessionCookies=true", api()))
                .json(&json!({ "email": "admin", "password": "admin-development-only" })),
            "signing in — is a development stack running?",
        )
        .await;

        let status = answer.status();

        // Name and value, dropping `Path`, `HttpOnly` and the rest: this is
        // being sent back, not stored, and a `Set-Cookie` attribute in a
        // `Cookie` header is not what a server reads.
        let cookie = answer
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .collect::<Vec<_>>()
            .join("; ");

        assert!(status.is_success(), "signing in: {status}");
        assert!(!cookie.is_empty(), "signing in set no cookie: {status}");
        Self { cookie }
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let answer = send(
            self.request(reqwest::Method::POST, path).json(&body),
            &format!("POST {path}"),
        )
        .await;

        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        assert!(status.is_success(), "POST {path} answered {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    async fn put(&self, path: &str, body: Value) -> Value {
        let answer = send(
            self.request(reqwest::Method::PUT, path).json(&body),
            &format!("PUT {path}"),
        )
        .await;

        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        assert!(status.is_success(), "PUT {path} answered {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    pub async fn get(&self, path: &str) -> Value {
        let answer = send(
            self.request(reqwest::Method::GET, path),
            &format!("GET {path}"),
        )
        .await;

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

/// A multipart body built by hand, with its content type.
///
/// **`reqwest::multipart::Form` is what stalls**, and that is measured rather
/// than supposed. Three runs stalled on a submit and never on any of the nine
/// JSON calls that precede it — the eighth submit once, the third, then the
/// second, the last of those with a client that had sent nothing before. The
/// same four parts by `curl` answer in 28 ms while the test is frozen, so the
/// Server and the shape are both fine; what is left between them is the encoder.
///
/// So the parts are laid out as bytes with a `Content-Length` the client cannot
/// get wrong, instead of a body it streams and computes.
fn multipart_body(parts: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = format!("aj{}", unique(""));
    let mut body = String::new();
    for (name, value) in parts {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    (
        format!("multipart/form-data; boundary={boundary}"),
        body.into_bytes(),
    )
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
    // **Each attempt announces itself before the request and times it.** A run
    // on 2026-08-22 stalled here for over ten minutes with the loop bounded at
    // forty attempts and a thirty-second client timeout in place — impossible if
    // the loop was turning — so an attempt has to say whether its request was
    // ever sent, and how long the answer took when it came.
    for attempt in 0..40 {
        eprintln!("  submitting, attempt {} of 40", attempt + 1);

        // **One opaque document, and a file name.** The language was a field
        // the Server read; it is a member of `props` now, and the Server named
        // pasted source from a table of seven extensions it no longer has — so
        // the sender names it or the submission is refused.
        let (content_type, body) = multipart_body(&[
            ("props", r#"{"type":"uva@1","language":"cpp11-gcc"}"#),
            ("code", source),
            ("fileName", "main.cpp"),
            ("sha256", &sha256_of(source)),
        ]);

        let sent = std::time::Instant::now();
        let answer = send(
            admin
                .request(reqwest::Method::POST, &path)
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body),
            "submitting",
        )
        .await;

        let status = answer.status();
        eprintln!("    answered {status} in {}ms", sent.elapsed().as_millis());
        if status.is_success() {
            let parsed: Value = answer.json().await.expect("a submission");
            return parsed["id"].as_str().expect("a submission id").to_owned();
        }

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
