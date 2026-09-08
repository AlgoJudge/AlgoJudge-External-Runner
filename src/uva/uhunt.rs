//! Reading uHunt: what the archive has judged, and which problem is which.
//!
//! Everything here is a read of somebody else's public API. The one design rule
//! is that a row we did not ask for is **ignored, not an error**: the account is
//! shared, so the window necessarily contains our own finished submissions and
//! anything a person did by hand while signed into it.

use std::time::Duration;

use serde::Deserialize;

/// One row of `GET /api/subs-user/{uid}/{min-sid}`.
///
/// Positional, seven elements, and the order is the whole contract:
/// `[sid, pid, verdict, runtimeMs, submittedAt, language, rank]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The submission id. **Monotonic**, which is what makes the cursor valid,
    /// and the only thing a pending entry is keyed on.
    pub sid: i64,
    /// uHunt's **internal** problem id — not the public number a person types.
    pub pid: i64,
    pub verdict_id: i64,
    pub runtime_ms: i64,
    pub submitted_at: i64,
    pub language_id: i64,
}

impl Row {
    /// Reads a row, or nothing.
    ///
    /// A short row is skipped rather than failing the poll: one malformed entry
    /// in a window of somebody else's submissions must not stop us reporting the
    /// verdicts that did arrive.
    fn read(value: &serde_json::Value) -> Option<Self> {
        let cells = value.as_array()?;
        let at = |i: usize| cells.get(i)?.as_i64();
        Some(Self {
            sid: at(0)?,
            pid: at(1)?,
            verdict_id: at(2)?,
            runtime_ms: at(3).unwrap_or(0),
            submitted_at: at(4).unwrap_or(0),
            language_id: at(5).unwrap_or(0),
        })
    }
}

/// The answer to `subs-user`: a name, and the rows.
#[derive(Debug, Deserialize)]
struct SubsUser {
    subs: Vec<serde_json::Value>,
}

/// Every row in a `subs-user` answer that could be read.
pub fn rows(body: &str) -> anyhow::Result<Vec<Row>> {
    let answer: SubsUser = serde_json::from_str(body)?;
    Ok(answer.subs.iter().filter_map(Row::read).collect())
}

/// Where to start asking from, given what is still outstanding.
///
/// **Derived at request time, never stored.** It is a function of the pending
/// set, so it advances by itself as the oldest submission completes; a cursor
/// kept beside that set would be a second piece of state that can disagree with
/// the first.
///
/// The `- 1` makes the inclusive-or-exclusive question moot — uHunt does not say
/// which `min-sid` is, the rows are filtered against the pending set anyway, and
/// both readings then give the same answer.
///
/// `None` means there is nothing outstanding, so **no request is made at all**.
/// The worker sleeps rather than polling an idle account.
pub fn cursor(pending: impl IntoIterator<Item = i64>) -> Option<i64> {
    pending.into_iter().min().map(|oldest| oldest - 1)
}

/// One problem, by its public number: `GET /api/p/num/{num}`.
///
/// Only `pid` is load-bearing — it is what a submission row carries, and it is
/// uHunt's own key rather than ours, so it is re-derived rather than depended on.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Problem {
    pub pid: i64,
    pub num: i64,
    pub title: String,
    /// `0` unavailable, `1` normal, `2` special judge.
    pub status: i64,
    /// The archive's own run-time limit, milliseconds. Informational: UVa
    /// enforces it, we do not, and a limit we do not enforce is a limit that lies.
    pub rtl: i64,
}

// ---------------------------------------------------------------- over the wire

/// Reading uHunt over HTTP.
///
/// Every call here is a plain public read. The base URL is a field rather than a
/// constant so a test can point it at a recorded stand-in without the production
/// code knowing it is talking to one.
pub struct Uhunt {
    http: reqwest::Client,
    base: String,
}

/// What uHunt's answer to `uname2uid` means.
///
/// Its own function so the rule can be tested without a request: the archive is
/// never a test dependency here, and this is a rule about a body rather than
/// about a protocol.
fn account_id(body: &str, username: &str) -> anyhow::Result<u64> {
    let uid: u64 = body
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("uHunt answered {body:?} for the account {username:?}"))?;
    if uid == 0 {
        anyhow::bail!(
            "uHunt does not know the account {username:?} — it answers 0 for a name it \
             has never seen, which is not an id to poll with"
        );
    }
    Ok(uid)
}

/// One path segment, and not a path.
///
/// **An operator's typo should not change the shape of a request.** The
/// username is configuration rather than anything a participant sends, so this
/// is a small hazard — but it is interpolated straight into a URL, and a name
/// carrying a slash or a question mark asked uHunt something other than what
/// this function is named for.
fn segment(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                let mut encoded = String::new();
                let mut buffer = [0u8; 4];
                for byte in c.encode_utf8(&mut buffer).as_bytes() {
                    encoded.push_str(&format!("%{byte:02X}"));
                }
                encoded
            }
        })
        .collect()
}

impl Uhunt {
    pub fn new(http: reqwest::Client, base: String) -> Self {
        Self { http, base }
    }

    async fn text(&self, path: &str) -> anyhow::Result<String> {
        let answer = self
            .http
            .get(format!("{}api/{path}", self.base))
            .send()
            .await?;
        let status = answer.status();
        let body = answer.text().await?;
        if !status.is_success() {
            anyhow::bail!("uHunt answered {status} for {path}");
        }
        Ok(body)
    }

    /// The account's numeric id, resolved once at start-up.
    ///
    /// **Zero is uHunt's word for "no such account", and it parses.** The error
    /// below could never fire for the case it names: a typo in
    /// `AJ_External__Username` resolved to uid 0, was cached, and every poll
    /// then asked `subs-user/0/…`, which answers no rows. Nothing ever matched,
    /// nothing was reported, and every submission aged out at
    /// `pending_timeout` as an infrastructure failure — while the solutions sat
    /// judged on the real account.
    pub async fn user_id(&self, username: &str) -> anyhow::Result<u64> {
        let body = self
            .text(&format!("uname2uid/{}", segment(username)))
            .await?;
        account_id(&body, username)
    }

    /// One problem by its public number — the number a person types.
    pub async fn problem(&self, number: i64) -> anyhow::Result<Problem> {
        let body = self.text(&format!("p/num/{number}")).await?;
        serde_json::from_str(&body)
            .map_err(|_| anyhow::anyhow!("onlinejudge.org has no problem {number}"))
    }

    /// Everything the account has submitted since `after`.
    ///
    /// The window is anchored to the **oldest** outstanding submission, so it
    /// grows with how long that one has been waiting rather than with how many
    /// are waiting. Rows that are not ours come back too; they are dropped by
    /// the caller, silently, because the account is shared.
    pub async fn since(&self, uid: u64, after: i64) -> anyhow::Result<Vec<Row>> {
        rows(&self.text(&format!("subs-user/{uid}/{after}")).await?)
    }

    /// **The live stream, held open until something happens.** uHunt answers at
    /// once when there is an event and otherwise holds the request for up to a
    /// minute, so this is a wait rather than a question.
    ///
    /// **Global, and lossy by design**: every event of every user goes through
    /// it and only the last hundred are kept, so a client that stops asking
    /// loses whatever passed meanwhile. That is why what comes back here is
    /// never read for a verdict — only as a reason to go and ask properly.
    ///
    /// The request's own timeout is raised above the client's minute, which
    /// would otherwise abort exactly the wait this is for.
    pub async fn poll(&self, after: i64, within: Duration) -> anyhow::Result<Vec<Event>> {
        let answer = self
            .http
            .get(format!("{}api/poll/{after}", self.base))
            .timeout(within + Duration::from_secs(15))
            .send()
            .await?;
        let status = answer.status();
        let body = answer.text().await?;
        if !status.is_success() {
            anyhow::bail!("uHunt answered {status} for the live stream");
        }
        Ok(serde_json::from_str(&body)?)
    }
}

/// One event from the live stream.
#[derive(Debug, serde::Deserialize)]
pub struct Event {
    pub id: i64,
    #[serde(default)]
    pub msg: EventAbout,
}

/// **Only the account matters here.** The event carries the verdict too, and
/// reading it would be the mistake this whole arrangement avoids: a stream that
/// drops what it cannot buffer is not a place to learn that somebody's
/// submission was judged.
#[derive(Debug, Default, serde::Deserialize)]
pub struct EventAbout {
    #[serde(default)]
    pub uid: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **uHunt answers `0` for a name it has never seen, and `0` parses.**
    ///
    /// So the "does not know the account" error could never fire for the case
    /// it was written for. A typo in `AJ_External__Username` became uid 0, was
    /// cached, and every poll asked `subs-user/0/…` — which answers no rows, so
    /// nothing matched and every submission aged out as an infrastructure
    /// failure while sitting judged on the real account.
    #[test]
    fn a_username_uhunt_does_not_know_is_not_account_zero() {
        let refused = account_id("0", "robto").unwrap_err().to_string();
        assert!(refused.contains("robto"), "{refused}");
        assert!(refused.contains("does not know"), "{refused}");

        // The error it could always fire for still does.
        assert!(account_id("<html>down</html>", "robot").is_err());
        // And a real id is still a real id.
        assert_eq!(account_id(" 12345\n", "robot").unwrap(), 12345);
    }

    /// A name is one segment of a path, whatever an operator typed into it.
    #[test]
    fn a_username_is_a_path_segment_and_not_a_path() {
        assert_eq!(segment("robot"), "robot");
        assert_eq!(segment("a.b-c_d~e"), "a.b-c_d~e");
        assert_eq!(segment("robot/../p/num/100"), "robot%2F..%2Fp%2Fnum%2F100");
        assert_eq!(segment("two words"), "two%20words");
        assert_eq!(segment("q?x=1"), "q%3Fx%3D1");
    }

    /// Captured from `GET /api/subs-user-last/{uid}/3` on 2026-08-13.
    const SUBS: &str = r#"{"name":"A Robot","uname":"robot","subs":[
        [28887350,4838,90,60,1698133496,5,520],
        [28887351,36,20,0,1698133500,3,-1],
        [28887352,36,70,120,1698133600,3,-1]
    ]}"#;

    #[test]
    fn a_row_is_read_by_position() {
        let read = rows(SUBS).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(
            read[0],
            Row {
                sid: 28887350,
                pid: 4838,
                verdict_id: 90,
                runtime_ms: 60,
                submitted_at: 1698133496,
                language_id: 5,
            }
        );
    }

    /// One unreadable entry must not cost the verdicts that did arrive.
    #[test]
    fn a_malformed_row_is_skipped_rather_than_failing_the_poll() {
        let body = r#"{"subs":[[1,2,90,0,0,3,0],"nonsense",[],[4,5,70,0,0,3,0]]}"#;
        let read = rows(body).unwrap();
        assert_eq!(read.iter().map(|r| r.sid).collect::<Vec<_>>(), vec![1, 4]);
    }

    #[test]
    fn the_cursor_is_one_below_the_oldest_outstanding() {
        assert_eq!(cursor([28887352, 28887350, 28887351]), Some(28887349));
    }

    /// The property the whole worker rests on: nothing pending, no request.
    #[test]
    fn nothing_pending_means_no_request() {
        assert_eq!(cursor(std::iter::empty()), None);
    }

    /// It advances by itself, which is why it is not stored.
    #[test]
    fn the_cursor_moves_as_the_oldest_completes() {
        let mut pending = vec![100, 200, 300];
        assert_eq!(cursor(pending.clone()), Some(99));
        pending.retain(|&sid| sid != 100);
        assert_eq!(cursor(pending.clone()), Some(199));
        pending.clear();
        assert_eq!(cursor(pending), None);
    }

    /// Captured from `GET /api/p/num/100` on 2026-08-16.
    #[test]
    fn a_problem_carries_the_internal_id_a_submission_row_uses() {
        let body = r#"{"pid":36,"num":100,"title":"The 3n + 1 problem","dacu":109011,
            "mrun":0,"mmem":1000000000,"nover":0,"sube":6949,"noj":0,"inq":0,"ce":139243,
            "rf":0,"re":100139,"ole":387,"tle":78877,"mle":5209,"wa":361824,"pe":6555,
            "ac":256349,"rtl":3000,"status":1,"rej":0}"#;
        let problem: Problem = serde_json::from_str(body).unwrap();
        assert_eq!(problem.pid, 36);
        assert_eq!(problem.num, 100);
        assert_eq!(problem.title, "The 3n + 1 problem");
        assert_eq!(problem.status, 1);
        assert_eq!(problem.rtl, 3000);
    }
}
