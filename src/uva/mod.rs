//! UVa Online Judge: everything that speaks to onlinejudge.org and uHunt.
//!
//! Split from the rest so that the two things this Runner does — hold a job for
//! the Server, and be a guest on somebody else's site — do not read as one.
//!
//! **This is one implementation of `crate::integration::Judge`, and currently
//! the only one.** Nothing above it knows the word "uHunt"; what the loop knows
//! is that a judge can be asked to submit something and asked what it decided.

pub mod language;
pub mod problem;
pub mod report;
pub mod site;
pub mod uhunt;
pub mod verdict;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::integration::{Judge, Language, Outcome, Refused, Setup};
use crate::pending::Entry;
use site::Site;
use uhunt::{Row, Uhunt};

/// The problem type this judge serves.
///
/// **A wire value shared with the Server, the Client and the documentation
/// site.** The Client resolves a result renderer from it and the Server stores
/// it without reading it, so it is not this repository's to rename.
pub const PROBLEM_TYPE: &str = "uva@1";

/// What a result document names as the judge.
pub const JUDGE: &str = "onlinejudge.org";

/// UVa Online Judge, reached through its web form and uHunt's read API.
pub struct Uva {
    site: Site,
    uhunt: Uhunt,
    /// The account submissions are made under, for resolving `uid`.
    username: String,
    /// Where the live stream was left, and whether its head has been taken yet.
    ///
    /// **Atomic because it is set from `submit`, which takes `&self`** — and it
    /// has to be set there: `/api/poll/0` answers with the last hundred events,
    /// which is history rather than news, so whoever takes that answer discards
    /// it. Taking it *after* a submission leaves discards the batch that may
    /// already carry that submission's own verdict, and then nothing else ever
    /// mentions it.
    poll_cursor: std::sync::atomic::AtomicI64,
    poll_primed: std::sync::atomic::AtomicBool,
    /// Resolved on first need, not at start-up.
    ///
    /// **A Runner that starts while the archive is down must still register and
    /// wait** — the specification says so, and resolving this eagerly made an
    /// unreachable uHunt into a Runner that never appeared in the manager panel
    /// at all. It is needed to poll, and polling only happens once something has
    /// been submitted.
    ///
    /// **Behind a lock because this judge is shared**, not because two tasks
    /// race for it meaningfully: the lock is never held across a request, so a
    /// simultaneous first need costs one duplicate lookup and nothing else.
    uid: Mutex<Option<u64>>,
    /// Public number to uHunt's internal id. Ours to re-derive, not to depend on.
    numbers: Mutex<BTreeMap<i64, i64>>,
}

impl Uva {
    pub fn new(site: Site, uhunt: Uhunt, username: String, uid: Option<u64>) -> Self {
        Self {
            site,
            uhunt,
            username,
            uid: Mutex::new(uid),
            numbers: Mutex::new(BTreeMap::new()),
            poll_cursor: std::sync::atomic::AtomicI64::new(0),
            poll_primed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The account's numeric id, resolved the first time it is wanted.
    async fn account(&self) -> Option<u64> {
        if let Some(uid) = *self.uid.lock().expect("the uid lock") {
            return Some(uid);
        }
        match self.uhunt.user_id(&self.username).await {
            Ok(uid) => {
                tracing::info!(uid, "resolved the archive account");
                *self.uid.lock().expect("the uid lock") = Some(uid);
                Some(uid)
            }
            // Not reaching uHunt says nothing about anybody's solution, and the
            // next cycle asks again.
            Err(e) => {
                tracing::warn!(%e, "could not resolve the archive account");
                None
            }
        }
    }
}

impl Judge for Uva {
    /// One row of uHunt's `subs-user` answer.
    type Answer = Row;

    fn problem_type(&self) -> &'static str {
        PROBLEM_TYPE
    }

    fn name(&self) -> &'static str {
        JUDGE
    }

    fn languages(&self) -> &'static [Language] {
        language::CATALOGUE
    }

    fn read(
        &self,
        props: Option<&serde_json::Value>,
        config: Option<&serde_json::Value>,
    ) -> anyhow::Result<Setup> {
        problem::read(props, config)
    }

    async fn problem(&self, number: i64) -> anyhow::Result<i64> {
        // **Read and released before the request.** Holding it across the await
        // would serialise every problem lookup behind the slowest one, and the
        // only cost of not holding it is that two tasks asking for the same
        // unknown number at the same instant both ask uHunt once.
        if let Some(pid) = self.numbers.lock().expect("the number lock").get(&number) {
            return Ok(*pid);
        }
        let found = self.uhunt.problem(number).await?;
        if found.status == 0 {
            anyhow::bail!(
                "onlinejudge.org lists problem {number} as unavailable, so it cannot be judged"
            );
        }
        self.numbers
            .lock()
            .expect("the number lock")
            .insert(number, found.pid);
        Ok(found.pid)
    }

    async fn submit(
        &self,
        number: i64,
        language: i64,
        source: &str,
        min_interval: Duration,
    ) -> Result<i64, Refused> {
        self.site
            .submit(number, language, source, min_interval)
            .await
    }

    /// Takes the stream's position, so that everything after it is news.
    ///
    /// `/api/poll/0` answers with the last hundred events — history — so
    /// whoever asks it discards a batch. That is why the caller says *when*:
    /// before a submission that nothing else is waiting behind.
    ///
    /// Failing costs nothing but promptness: the cursor keeps whatever it had,
    /// the interval net still runs, and the next batch asks again.
    async fn note_where_the_channel_is(&self) {
        use std::sync::atomic::Ordering;
        match self.uhunt.poll(0, Duration::from_secs(2)).await {
            Ok(events) => {
                if let Some(last) = events.last() {
                    self.poll_cursor.store(last.id, Ordering::SeqCst);
                    self.poll_primed.store(true, Ordering::SeqCst);
                }
            }
            Err(e) => tracing::debug!(%e, "could not read where the archive's live stream is"),
        }
    }

    /// **uHunt holds the request until something happens**, so this is a wait,
    /// not a poll — and one request a minute rather than three.
    ///
    /// The stream is global and keeps only the last hundred events, so what
    /// arrives is read for one thing alone: whether the account we submit under
    /// appears. The verdict on the event is deliberately ignored; a stream that
    /// silently drops what it cannot buffer would turn a lost event into a
    /// submission that hangs until it times out.
    async fn wait_for_a_sign(&self, within: Duration) -> bool {
        let Some(uid) = self.account().await else {
            tokio::time::sleep(within).await;
            return false;
        };

        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let cursor = self.poll_cursor.load(std::sync::atomic::Ordering::SeqCst);
            match self.uhunt.poll(cursor, left).await {
                Ok(events) => {
                    if let Some(last) = events.last() {
                        self.poll_cursor
                            .store(last.id, std::sync::atomic::Ordering::SeqCst);
                    }
                    if events.iter().any(|event| event.msg.uid == uid) {
                        return true;
                    }
                }
                // The stream is an accelerator: losing it costs promptness and
                // nothing else, because the interval net still runs.
                Err(e) => {
                    tracing::debug!(%e, "the archive's live stream did not answer");
                    tokio::time::sleep(left.min(Duration::from_secs(5))).await;
                }
            }
        }
    }

    /// One request, however many submissions are outstanding.
    ///
    /// The window is anchored to the **oldest** outstanding submission, so it
    /// grows with how long that one has been waiting rather than with how many
    /// are waiting. Rows that are not ours come back too and are dropped by the
    /// caller, silently, because the account is shared.
    async fn answers(&self, outstanding: &[i64]) -> anyhow::Result<Vec<Row>> {
        let Some(after) = uhunt::cursor(outstanding.iter().copied()) else {
            return Ok(Vec::new());
        };
        // Not reaching uHunt says nothing about anybody's solution. It is
        // already reported by `account`, and the next cycle asks again.
        let Some(uid) = self.account().await else {
            return Ok(Vec::new());
        };
        self.uhunt.since(uid, after).await
    }

    fn id_of(&self, answer: &Row) -> i64 {
        answer.sid
    }

    fn problem_of(&self, answer: &Row) -> i64 {
        answer.pid
    }

    fn evidence(&self, answer: &Row) -> String {
        format!(
            "row: [{},{},{},{},{},{}]",
            answer.sid,
            answer.pid,
            answer.verdict_id,
            answer.runtime_ms,
            answer.submitted_at,
            answer.language_id
        )
    }

    fn outcome(&self, answer: &Row) -> Outcome {
        verdict::of(answer.verdict_id)
    }

    fn details(
        &self,
        entry: &Entry,
        answer: &Row,
        verdict: &str,
        abbreviation: &str,
        solved: bool,
    ) -> serde_json::Value {
        report::details(entry, answer, verdict, abbreviation, solved)
    }

    fn details_of_failure(&self, entry: &Entry, id: i64, why: &str) -> serde_json::Value {
        report::details_of_failure(entry, id, why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::Chosen;

    /// A judge that reaches nothing. Every method exercised below is decided
    /// from the catalogue and the arguments, so no request is made.
    fn offline() -> Uva {
        Uva::new(
            Site::new(
                "https://onlinejudge.test/".into(),
                "robot".into(),
                "not-a-real-password".into(),
            )
            .expect("a site client"),
            Uhunt::new(reqwest::Client::new(), "https://uhunt.test/".into()),
            "robot".into(),
            Some(1),
        )
    }

    fn setup(languages: &[&str]) -> Setup {
        Setup {
            languages: languages.iter().map(|l| (*l).to_owned()).collect(),
            number: 100,
            accepted: vec!["AC".to_owned()],
        }
    }

    /// **Not out of either document.** `uva@1` offers what onlinejudge.org
    /// offers, which is the same six for every problem in the archive — holding
    /// them per problem meant writing six numbers into every import, and an
    /// import that wrote none produced a problem nobody could submit to.
    #[test]
    fn the_languages_come_from_the_type() {
        assert_eq!(
            offline().language(&setup(&[]), Some("cpp11-gcc")),
            Chosen::Accepted(5)
        );
    }

    /// **The manager narrowed the list, and this is outside it.**
    ///
    /// A verdict rather than an infrastructure failure, and the same one
    /// `standard-io@1` gives: the participant chose it, their code may be
    /// perfect, and what they broke is a rule of the activity. Two problem
    /// types answering this differently would make the verdict a property of
    /// who judged rather than of what happened.
    #[test]
    fn a_language_the_assignment_excluded_is_not_allowed_rather_than_unknown() {
        let judge = offline();
        let narrowed = setup(&["python3"]);

        // In the archive's six, and outside what this assignment allows.
        assert_eq!(
            judge.language(&narrowed, Some("cpp11-gcc")),
            Chosen::NotAllowed {
                wanted: "cpp11-gcc".into(),
                allowed: vec!["python3".into()],
            },
        );
        assert_eq!(
            judge.language(&narrowed, Some("python3")),
            Chosen::Accepted(6)
        );

        // Still not submittable, and still for the other reason: an assignment
        // narrowing the list does not make an unknown language into a rule.
        assert!(matches!(
            judge.language(&narrowed, Some("rust")),
            Chosen::NotSubmittable(_)
        ));
    }

    /// An assignment that names none allows all six. **Not none** — an
    /// assignment allowing nothing would be one nobody could submit to.
    #[test]
    fn an_assignment_that_names_no_languages_allows_them_all() {
        let judge = offline();
        for id in language::ids() {
            assert!(
                matches!(judge.language(&setup(&[]), Some(id)), Chosen::Accepted(_)),
                "{id} was refused",
            );
        }
    }

    /// A language the archive does not offer is refused by name, with the list.
    #[test]
    fn an_unlisted_language_is_refused_and_says_what_is_on_offer() {
        let judge = offline();
        // Not a language the archive offers at all — the platform's fault, not
        // the participant's, so it stays an infrastructure failure.
        let Chosen::NotSubmittable(refused) = judge.language(&setup(&[]), Some("rust")) else {
            panic!("a language onlinejudge.org does not offer must not be submittable");
        };
        assert!(refused.contains("rust"), "{refused}");
        assert!(refused.contains("cpp11-gcc"), "{refused}");
        assert!(refused.contains("onlinejudge.org"), "{refused}");

        let Chosen::NotSubmittable(missing) = judge.language(&setup(&[]), None) else {
            panic!("a submission naming no language cannot be forwarded");
        };
        assert!(missing.contains("names no language"), "{missing}");
    }

    /// **The four accessors the loop reads an answer through**, pinned to the
    /// members they are supposed to name.
    ///
    /// This is the one seam where a mix-up is silent: `id_of` and `problem_of`
    /// returning each other's field would make every answer a `Disagrees`, and a
    /// queue that never drains looks exactly like a judge that is slow.
    #[test]
    fn an_answer_is_read_through_the_members_it_names() {
        let judge = offline();
        let row = Row {
            sid: 31254724,
            pid: 36,
            verdict_id: 70,
            runtime_ms: 120,
            submitted_at: 1786854437,
            language_id: 5,
        };

        assert_eq!(
            judge.id_of(&row),
            31254724,
            "the correlation key is the sid"
        );
        assert_eq!(
            judge.problem_of(&row),
            36,
            "the check is on uHunt's own pid"
        );
        assert_eq!(
            judge.outcome(&row),
            Outcome::Judged {
                verdict: "WrongAnswer",
                abbreviation: "WA"
            }
        );
        assert_eq!(
            judge.evidence(&row),
            "row: [31254724,36,70,120,1786854437,5]",
            "the row verbatim, in the order uHunt sends it"
        );
    }

    /// Both are wire values the Server, the Client and the documentation site
    /// share, so they are pinned here rather than left to a rename.
    #[test]
    fn the_type_and_the_judge_are_the_names_the_rest_of_the_product_uses() {
        let judge = offline();
        assert_eq!(judge.problem_type(), "uva@1");
        assert_eq!(judge.name(), "onlinejudge.org");
    }
}
