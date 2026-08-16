//! Reading uHunt: what the archive has judged, and which problem is which.
//!
//! Everything here is a read of somebody else's public API. The one design rule
//! is that a row we did not ask for is **ignored, not an error**: the account is
//! shared, so the window necessarily contains our own finished submissions and
//! anything a person did by hand while signed into it.

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

#[cfg(test)]
mod tests {
    use super::*;

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

impl Uhunt {
    pub fn new(http: reqwest::Client, base: String) -> Self {
        Self { http, base }
    }

    async fn text(&self, path: &str) -> anyhow::Result<String> {
        let answer = self.http.get(format!("{}api/{path}", self.base)).send().await?;
        let status = answer.status();
        let body = answer.text().await?;
        if !status.is_success() {
            anyhow::bail!("uHunt answered {status} for {path}");
        }
        Ok(body)
    }

    /// The account's numeric id, resolved once at start-up.
    pub async fn user_id(&self, username: &str) -> anyhow::Result<u64> {
        let body = self.text(&format!("uname2uid/{username}")).await?;
        body.trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("uHunt does not know the account {username:?}"))
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
}
