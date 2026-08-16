//! What the archive said, turned into what the Server stores.
//!
//! **The line worth caring about runs between two rows.** `SubmissionError` and
//! `CannotBeJudged` are the external judge failing to form an opinion — not the
//! participant being wrong. Reporting either as a wrong answer marks somebody
//! down for our infrastructure, so they leave here as an infrastructure failure
//! and the Server refuses to score one.

/// What the Runner does with a submission row it recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The archive judged it. `verdict` is stored verbatim; `solved` decides the
    /// score against the activity's list of accepted verdicts.
    Judged {
        /// The canonical long name, for the column filters and rankings key on.
        verdict: &'static str,
        /// The two-letter name a UVa user recognises, for the result screen.
        abbreviation: &'static str,
    },
    /// Not judged yet. Keep waiting; this is not an answer.
    Pending,
    /// The judge never formed an opinion. Never a verdict, never a score.
    Failed {
        reason: &'static str,
        /// Whether asking again could ever produce a different answer.
        ///
        /// `CannotBeJudged` is a property of the problem — the archive holds no
        /// tests for it — so a retry is four more submissions to somebody else's
        /// site for the same answer.
        permanent: bool,
    },
}

/// uHunt's verdict id, as it appears at index 2 of a submission row.
///
/// Read from <https://onlinejudge.org/index.php?option=com_content&task=view&id=16>
/// on 2026-08-13. Anything not listed is treated as not-yet-judged rather than
/// guessed at: a number we do not know is not evidence that a person was wrong.
pub fn of(id: i64) -> Outcome {
    match id {
        90 => judged("Accepted", "AC"),
        80 => judged("PresentationError", "PE"),
        70 => judged("WrongAnswer", "WA"),
        60 => judged("MemoryLimitExceeded", "ML"),
        50 => judged("TimeLimitExceeded", "TL"),
        45 => judged("OutputLimitExceeded", "OL"),
        40 => judged("RuntimeError", "RE"),
        35 => judged("RestrictedFunction", "RF"),
        30 => judged("CompilationError", "CE"),

        // In queue, and the row that appears within seconds of submitting.
        0 | 20 => Outcome::Pending,

        10 => Outcome::Failed {
            reason: "onlinejudge.org refused the submission (submission error)",
            permanent: false,
        },
        15 => Outcome::Failed {
            reason: "onlinejudge.org has no tests for this problem (can't be judged)",
            permanent: true,
        },

        _ => Outcome::Pending,
    }
}

const fn judged(verdict: &'static str, abbreviation: &'static str) -> Outcome {
    Outcome::Judged {
        verdict,
        abbreviation,
    }
}

/// Whether a judged verdict counts as solved **here**.
///
/// The list comes from the problem's configuration, not from this file: a
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_verdict_is_mapped() {
        for (id, verdict, abbreviation) in [
            (90, "Accepted", "AC"),
            (80, "PresentationError", "PE"),
            (70, "WrongAnswer", "WA"),
            (60, "MemoryLimitExceeded", "ML"),
            (50, "TimeLimitExceeded", "TL"),
            (45, "OutputLimitExceeded", "OL"),
            (40, "RuntimeError", "RE"),
            (35, "RestrictedFunction", "RF"),
            (30, "CompilationError", "CE"),
        ] {
            assert_eq!(
                of(id),
                Outcome::Judged {
                    verdict,
                    abbreviation
                },
                "uHunt {id}"
            );
        }
    }

    #[test]
    fn queued_is_not_an_answer() {
        assert_eq!(of(0), Outcome::Pending);
        assert_eq!(of(20), Outcome::Pending);
    }

    /// The whole reason this module is separate from the poller.
    #[test]
    fn the_judge_failing_is_never_a_verdict() {
        let Outcome::Failed {
            permanent: retryable_one,
            ..
        } = of(10)
        else {
            panic!("submission error was reported as a verdict");
        };
        assert!(!retryable_one, "a submission error may be tried again");

        let Outcome::Failed {
            permanent: permanent_one,
            ..
        } = of(15)
        else {
            panic!("can't-be-judged was reported as a verdict");
        };
        assert!(
            permanent_one,
            "can't-be-judged must not be retried: the archive has no tests for \
             that problem, so asking again is four more submissions to somebody \
             else's site for the same answer"
        );
    }

    /// A number nobody has seen is not evidence that a person was wrong.
    #[test]
    fn an_unknown_id_waits_rather_than_guessing() {
        assert_eq!(of(999), Outcome::Pending);
        assert_eq!(of(-1), Outcome::Pending);
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
