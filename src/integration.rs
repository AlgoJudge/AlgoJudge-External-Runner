//! The boundary an external judging system is reached through.
//!
//! **This repository forwards work; it does not judge it.** What differs between
//! one external judge and the next is how a submission is handed over, how an
//! answer is asked for, and what its verdicts are called. What does not differ is
//! everything around that: claiming a job, holding its lease while somebody else
//! thinks, matching an answer to what was sent, and reporting it.
//!
//! So the loop in `run.rs` is written against this trait and names no archive.
//! **UVa Online Judge is the only implementation there is** — `crate::uva` — and
//! a second one is a module beside it rather than a fork of the loop.
//!
//! **Generic rather than `dyn`.** There is one implementation, the selection
//! happens once at start-up, and object safety would buy nothing; the methods
//! return `impl Future + Send` because the loop is driven from a spawned task.

use std::time::Duration;

use serde::Deserialize;

use crate::pending::Entry;

/// One language a judge accepts.
pub struct Language {
    /// The product's id, as a submission carries it.
    pub id: &'static str,
    /// What a person reads. The judge's own compiler and version.
    pub label: &'static str,
    /// The value the judge's own submit form or API wants. Its numbering, not
    /// ours.
    pub number: i64,
}

/// The problem, as this Runner needs it, out of the two documents describing it.
///
/// **The number comes from the version and the scoring from the assignment**,
/// and neither substitutes for the other: a problem attached to three courses is
/// one archive number and three ways of counting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// Which languages this assignment allows. Empty means all the judge offers.
    pub languages: Vec<String>,
    /// The judge's public number for the problem — what a person types.
    pub number: i64,
    /// Which verdicts count as solved. Never empty: a problem nobody can pass is
    /// a configuration mistake, not a strict activity.
    pub accepted: Vec<String>,
}

/// What happened when a submission named a language.
#[derive(Debug, PartialEq, Eq)]
pub enum Chosen {
    /// The judge's own value for it.
    Accepted(i64),
    /// **The manager narrowed the list and this is outside it.** A verdict, not
    /// an infrastructure failure: the participant chose it, their code may be
    /// perfect, and what they broke is a rule of the activity — which is what
    /// `standard-io@1` reports for the same mistake, and the two types must not
    /// answer it differently.
    NotAllowed {
        wanted: String,
        allowed: Vec<String>,
    },
    /// Not a language the judge offers at all, or none named. The participant
    /// chose from a list the platform gave them, so this is the platform's
    /// fault: an infrastructure failure, and the submission stays rejudgeable.
    NotSubmittable(String),
}

/// What the Runner does with an answer it recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The judge decided. `verdict` is stored verbatim; `solved` decides the
    /// score against the activity's list of accepted verdicts.
    Judged {
        /// The canonical long name, for the column filters and rankings key on.
        verdict: &'static str,
        /// The short name a user of that judge recognises, for the result screen.
        abbreviation: &'static str,
    },
    /// Not judged yet. Keep waiting; this is not an answer.
    Pending,
    /// The judge never formed an opinion. Never a verdict, never a score.
    Failed {
        reason: &'static str,
        /// Whether asking again could ever produce a different answer.
        permanent: bool,
    },
}

/// Why a submission did not produce an id.
#[derive(Debug)]
pub enum Refused {
    /// The session is gone — re-establish it **once** and try again.
    SessionLapsed,
    /// **The judge said it received the submission and did not name it.**
    ///
    /// Its own variant rather than a `Site`, because the two earn opposite
    /// treatment and were given the same one until 2026-08-31: this submission
    /// is **on the account**. A second attempt is a second row on a third
    /// party's history for one participant's one attempt, and no id will ever
    /// match this job to the answer it produces. The job fails and stays
    /// rejudgeable — by a person, who can look at the account first.
    AcceptedWithoutAnId,
    /// Anything else. Not retried: a second attempt at a submission the judge
    /// has already refused is a duplicate to a third party.
    Site(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionLapsed => write!(f, "the session with the judge had lapsed"),
            Self::AcceptedWithoutAnId => write!(
                f,
                "the judge received the submission and did not say which id it gave it"
            ),
            Self::Site(why) => write!(f, "the judge refused the submission: {why}"),
        }
    }
}

/// What a submission the activity's rules refuse is called.
///
/// **The same word `standard-io@1` uses**, and that is the point: a participant
/// who chose a language the manager excluded should read the same verdict
/// whichever Runner would have judged it. The judge never sees this submission —
/// nothing is sent — so there is no external verdict to report.
pub const POLICY_VIOLATION: &str = "PolicyViolation";

/// Whether a judged verdict counts as solved **here**.
///
/// The list comes from the problem's configuration, not from any judge: a
/// contest counts only `AC`, while a course may reasonably accept a correct
/// answer with sloppy whitespace. Expressed as a list rather than as a fraction,
/// so the score stays binary and every number in it comes from a rule somebody
/// wrote down rather than from a judgement we invented about somebody else's
/// verdict.
pub fn solved(abbreviation: &str, accepted: &[String]) -> bool {
    accepted
        .iter()
        .any(|a| a.eq_ignore_ascii_case(abbreviation))
}

// ------------------------------------------------- what the assignment decides

/// The assignment's document: how this course judges the problem.
///
/// **Not the judge's half.** Which problem this is belongs to whichever judge
/// holds it, and is read by the implementation; how a course counts the answer
/// is the product's, is spelt the same for every judge, and is read here.
#[derive(Debug, Deserialize)]
struct Judging {
    scoring: Option<Scoring>,
    /// Which of the judge's languages this assignment allows. Empty means it
    /// said nothing, which allows all of them.
    #[serde(default)]
    languages: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scoring {
    #[serde(default)]
    accepted_verdicts: Vec<String>,
}

/// The default, and the only one: an accepted answer.
///
/// A course widens it deliberately. Nothing widens it by accident.
const STRICT: &str = "AC";

/// The allowed languages and the accepted verdicts, out of the assignment.
///
/// **Absent is allowed.** An assignment that says nothing about scoring gets the
/// strict default, which is the answer a course would have written anyway;
/// refusing would make every attachment carry a document to say "as usual".
pub fn judging(config: Option<&serde_json::Value>) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let judging = config
        .map(|c| serde_json::from_value::<Judging>(c.clone()))
        .transpose()
        .map_err(|e| anyhow::anyhow!("the assignment's configuration cannot be read: {e}"))?;

    let languages = judging
        .as_ref()
        .map(|j| j.languages.clone())
        .unwrap_or_default();

    let accepted = match judging.and_then(|j| j.scoring) {
        Some(scoring) if !scoring.accepted_verdicts.is_empty() => scoring.accepted_verdicts,
        _ => vec![STRICT.to_owned()],
    };

    Ok((languages, accepted))
}

// ------------------------------------------------------------------- the judge

/// One external judging system.
///
/// The methods split into three groups: what the judge *is* (its problem type,
/// its name, its languages), what it is *asked* (read a problem, submit, collect
/// answers), and how an answer is *read* (its ids, its verdict, the documents it
/// leaves behind). The loop uses nothing else.
pub trait Judge: Send + Sync {
    /// What the judge says about one submission it is holding.
    ///
    /// Opaque to the loop: it is passed straight back to the methods below,
    /// which is what lets one judge answer in rows and another in objects.
    type Answer: Send;

    /// The problem type this judge serves — `uva@1` for UVa Online Judge.
    fn problem_type(&self) -> &'static str;

    /// What a result document names as the judge, and what a log line calls it.
    fn name(&self) -> &'static str;

    /// Every language this judge accepts.
    fn languages(&self) -> &'static [Language];

    /// Which problem this is, and how this activity counts it.
    ///
    /// `props` is the version's document — identity, the judge's own — and
    /// `config` is the assignment's, which `judging` above reads the same way
    /// for every judge.
    fn read(
        &self,
        props: Option<&serde_json::Value>,
        config: Option<&serde_json::Value>,
    ) -> anyhow::Result<Setup>;

    /// The judge's internal id for a problem, which its answers carry.
    ///
    /// Cached by the implementation: it is somebody else's key, ours to
    /// re-derive rather than to depend on.
    fn problem(
        &mut self,
        number: i64,
    ) -> impl std::future::Future<Output = anyhow::Result<i64>> + Send;

    /// Hands one submission over and returns the judge's id for it.
    fn submit(
        &self,
        number: i64,
        language: i64,
        source: &str,
        min_interval: Duration,
    ) -> impl std::future::Future<Output = Result<i64, Refused>> + Send;

    /// Everything the judge has to say about the submissions still outstanding.
    ///
    /// **One request, however many are waiting.** Anything the judge volunteers
    /// that we did not send comes back too and is dropped by the caller: an
    /// account may be shared, and an answer we did not ask for is not an error.
    fn answers(
        &mut self,
        outstanding: &[i64],
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Self::Answer>>> + Send;

    /// The judge's submission id — the correlation key, and the only one.
    fn id_of(&self, answer: &Self::Answer) -> i64;

    /// The judge's internal problem id, to verify the answer is about what we sent.
    fn problem_of(&self, answer: &Self::Answer) -> i64;

    /// The answer verbatim, for the evidence log.
    ///
    /// When the verdict comes from somebody else's judge, the answers themselves
    /// are the only reply to a dispute about what that judge said. A parsed
    /// summary is not evidence.
    fn evidence(&self, answer: &Self::Answer) -> String;

    /// What the answer means.
    fn outcome(&self, answer: &Self::Answer) -> Outcome;

    /// The result document for a judged submission.
    fn details(
        &self,
        entry: &Entry,
        answer: &Self::Answer,
        verdict: &str,
        abbreviation: &str,
        solved: bool,
    ) -> serde_json::Value;

    /// The result document for a submission that was never judged.
    fn details_of_failure(&self, entry: &Entry, id: i64, why: &str) -> serde_json::Value;

    /// Every language id, for a refusal that says what is on offer.
    fn language_ids(&self) -> Vec<&'static str> {
        self.languages().iter().map(|l| l.id).collect()
    }

    /// The judge's number for a language, or a refusal naming what is on offer.
    ///
    /// Refused here as well as in the Client, because a rule only the Client
    /// applies is a rule a devtools console turns off.
    ///
    /// **Against the judge's own catalogue**, not against something the problem
    /// carried: a judge offers what it offers, which is the same list for every
    /// problem it holds.
    fn language(&self, setup: &Setup, wanted: Option<&str>) -> Chosen {
        let Some(wanted) = wanted else {
            return Chosen::NotSubmittable(format!(
                "the submission names no language, and {} needs one",
                self.name()
            ));
        };

        let Some(known) = self.languages().iter().find(|l| l.id == wanted) else {
            return Chosen::NotSubmittable(format!(
                "{} does not accept {wanted:?}; it accepts {:?}",
                self.name(),
                self.language_ids()
            ));
        };

        // **The manager's subset, and the reason this is a verdict.** An empty
        // list is the assignment saying nothing, which allows all of them — not
        // allowing none, which would be an assignment nobody could submit to.
        if !setup.languages.is_empty() && !setup.languages.iter().any(|l| l == wanted) {
            return Chosen::NotAllowed {
                wanted: wanted.to_owned(),
                allowed: setup.languages.clone(),
            };
        }

        Chosen::Accepted(known.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Option<serde_json::Value> {
        Some(serde_json::from_str(text).unwrap())
    }

    /// **Absent is allowed.** An assignment that says nothing about scoring gets
    /// the strict default, which is what a course would have written anyway;
    /// refusing would make every attachment carry a document to say "as usual".
    #[test]
    fn strict_is_the_default_when_the_assignment_says_nothing() {
        assert_eq!(judging(None).unwrap().1, vec!["AC".to_owned()]);
        assert_eq!(
            judging(document("{}").as_ref()).unwrap().1,
            vec!["AC".to_owned()]
        );
    }

    /// The assignment's document is the one a course varies: the same imported
    /// problem, counted two ways.
    #[test]
    fn an_assignment_may_widen_what_counts_as_solved() {
        let lenient = document(r#"{"scoring":{"acceptedVerdicts":["AC","PE"]}}"#);
        assert_eq!(
            judging(lenient.as_ref()).unwrap().1,
            vec!["AC".to_owned(), "PE".to_owned()]
        );
    }

    /// An assignment that names none allows them all. **Not none** — an
    /// assignment allowing nothing would be one nobody could submit to.
    #[test]
    fn an_assignment_may_narrow_the_languages() {
        let narrowed = document(r#"{"languages":["python3"]}"#);
        assert_eq!(
            judging(narrowed.as_ref()).unwrap().0,
            vec!["python3".to_owned()]
        );
        assert!(judging(None).unwrap().0.is_empty(), "silence allows all");
    }

    #[test]
    fn what_counts_as_solved_is_configuration() {
        let strict = vec!["AC".to_owned()];
        let lenient = vec!["AC".to_owned(), "PE".to_owned()];

        assert!(solved("AC", &strict));
        assert!(!solved("PE", &strict));
        assert!(solved("PE", &lenient));
        // A configuration file is written by a person, so case is not a trap.
        assert!(solved("ac", &strict));
    }
}
