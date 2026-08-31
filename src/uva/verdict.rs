//! What the archive said, turned into what the Server stores.
//!
//! **The line worth caring about runs between two rows.** `SubmissionError` and
//! `CannotBeJudged` are the external judge failing to form an opinion — not the
//! participant being wrong. Reporting either as a wrong answer marks somebody
//! down for our infrastructure, so they leave here as an infrastructure failure
//! and the Server refuses to score one.
//!
//! The distinction itself is not UVa's — every external judge has a way of
//! failing to decide — so `Outcome` lives in `crate::integration`. What is UVa's
//! is the numbering below.

use crate::integration::Outcome;

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

/// The verdict id a compilation failure carries, which is the one thing the
/// result document's `compilation` member is derived from.
pub const COMPILATION_ERROR: i64 = 30;

const fn judged(verdict: &'static str, abbreviation: &'static str) -> Outcome {
    Outcome::Judged {
        verdict,
        abbreviation,
    }
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

    /// The one id the result document reads directly rather than through
    /// `of`, so it is pinned to the verdict it is supposed to mean.
    #[test]
    fn the_compilation_error_id_is_the_one_that_maps_to_ce() {
        assert_eq!(
            of(COMPILATION_ERROR),
            Outcome::Judged {
                verdict: "CompilationError",
                abbreviation: "CE"
            }
        );
    }
}
