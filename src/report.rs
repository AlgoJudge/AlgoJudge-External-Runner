//! The two artefacts a finished submission leaves behind.
//!
//! **`details` is where the run time lives** — the same place `standard-io` puts
//! its per-test table, read by the result renderer through the ordinary
//! attachment mechanism. Nothing new on the Server, and nothing type-specific:
//! `Result.Extra` stays what it is, a scoreboard row that is public by
//! construction and withheld under a freeze, which is the wrong disclosure rule
//! for a person's own submission.
//!
//! **`log` is the evidence.** It matters more here than for a local problem:
//! when the verdict comes from somebody else's judge, the dispute *"UVa says my
//! submission was AC and AlgoJudge shows WA"* is one a manager will eventually
//! have to answer, and the only way to answer it is to have recorded what the
//! archive actually returned, when, and for which submission id. A parsed
//! summary is not evidence; the rows are.

use serde_json::json;

use crate::pending::Entry;
use crate::uva::uhunt::Row;

/// The result document, as the `uva@1` renderer reads it.
///
/// **`type` is one string** — `uva@1` — since 2026-08-22. It was `kind` beside
/// `version`, which was one of four spellings of a convention decided as one
/// string in August; a convention with four spellings is not a convention.
///
/// **Timestamps are seconds since the epoch, and say so in their names.** The
/// specification's example wrote ISO strings; producing those would mean either
/// a date library for two fields or hand-rolled calendar arithmetic, and a
/// number that cannot be misread is worth more than a string that looks
/// familiar. `submittedAtUnix` is the archive's own, from the submission row.
pub fn details(
    entry: &Entry,
    row: &Row,
    verdict: &str,
    abbreviation: &str,
    solved: bool,
) -> serde_json::Value {
    json!({
        "type": "uva@1",
        "score": if solved { 1 } else { 0 },
        "maxScore": 1,
        "external": {
            "judge": "onlinejudge.org",
            "problemNumber": entry.problem_number,
            "submissionId": row.sid,
            "verdictId": row.verdict_id,
            "verdict": verdict,
            "verdictAbbr": abbreviation,
            "languageId": row.language_id,
            "runtimeMs": row.runtime_ms,
            "submittedAtUnix": row.submitted_at,
            "judgedAtUnix": now(),
        },
        // **No compiler log, and that is measured rather than assumed**: the site
        // provides compiler output by email and excludes warnings, so there is
        // nothing here to fetch. The status is all that can honestly be said.
        "compilation": { "status": if row.verdict_id == 30 { "ERROR" } else { "OK" } },
    })
}

/// The document for a submission that was never judged.
///
/// Written for the same reason as the one above: an infrastructure failure is
/// the case somebody will ask about, and "it did not work" is not an answer.
pub fn details_of_failure(entry: &Entry, sid: i64, why: &str) -> serde_json::Value {
    json!({
        "type": "uva@1",
        "external": {
            "judge": "onlinejudge.org",
            "problemNumber": entry.problem_number,
            "submissionId": sid,
        },
        "failure": why,
    })
}

/// Seconds since the epoch, or zero on a clock set before 1970.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn entry() -> Entry {
        Entry {
            job_id: "job".into(),
            lease_token: "token".into(),
            problem_number: 100,
            pid: 36,
            language_id: 1,
            sent: Instant::now(),
            announced: true,
            accepted: vec!["AC".to_owned()],
            trail: vec![
                "submitted problem 100 as language 1".into(),
                "redirect: …mosmsg=Submission+received+with+ID+31254724".into(),
                "sid: 31254724".into(),
            ],
        }
    }

    fn row(verdict_id: i64, runtime_ms: i64) -> Row {
        Row {
            sid: 31254724,
            pid: 36,
            verdict_id,
            runtime_ms,
            submitted_at: 1786854437,
            language_id: 1,
        }
    }

    #[test]
    fn the_run_time_is_in_the_document_the_renderer_reads() {
        let document = details(&entry(), &row(90, 60), "Accepted", "AC", true);
        assert_eq!(document["external"]["runtimeMs"], 60);
        assert_eq!(document["external"]["submissionId"], 31254724);
        assert_eq!(document["external"]["verdictAbbr"], "AC");
        assert_eq!(document["score"], 1);
        assert_eq!(document["maxScore"], 1, "binary, on a scale of one");
    }

    /// A verdict the activity does not count is still the archive's verdict.
    #[test]
    fn a_verdict_that_does_not_count_keeps_its_name_and_loses_its_point() {
        let document = details(&entry(), &row(80, 20), "PresentationError", "PE", false);
        assert_eq!(document["score"], 0);
        assert_eq!(document["external"]["verdict"], "PresentationError");
        assert_eq!(document["external"]["verdictAbbr"], "PE");
    }

    #[test]
    fn a_compile_error_is_the_only_thing_that_marks_compilation() {
        assert_eq!(
            details(&entry(), &row(30, 0), "CompilationError", "CE", false)["compilation"]
                ["status"],
            "ERROR"
        );
        assert_eq!(
            details(&entry(), &row(70, 10), "WrongAnswer", "WA", false)["compilation"]["status"],
            "OK"
        );
    }

    /// Named so it cannot be read as an ISO string by a renderer that guesses.
    #[test]
    fn the_timestamps_are_seconds_and_are_named_as_such() {
        let document = details(&entry(), &row(90, 60), "Accepted", "AC", true);
        assert_eq!(document["external"]["submittedAtUnix"], 1786854437);
        assert!(document["external"]["judgedAtUnix"].as_i64().unwrap() > 1_700_000_000);
        assert!(
            document["external"]["submittedAt"].is_null(),
            "an ISO-looking name must not exist beside the seconds"
        );
    }

    /// The trail is what answers "UVa said AC and you showed WA".
    #[test]
    fn the_log_carries_the_rows_verbatim_and_the_id_they_were_matched_on() {
        let mut entry = entry();
        entry
            .trail
            .push("row: [31254724,36,20,0,1786854437,1,-1]".into());
        entry
            .trail
            .push("row: [31254724,36,70,0,1786854437,1,-1]".into());
        let log = entry.trail.join("\n");

        assert!(
            log.contains("31254724"),
            "the correlation key is in the log"
        );
        assert!(
            log.contains("[31254724,36,70,0,1786854437,1,-1]"),
            "the row, verbatim"
        );
        assert!(log.contains("redirect:"), "where the id came from");
    }

    #[test]
    fn a_submission_that_was_never_judged_still_says_which_one_it_was() {
        let document = details_of_failure(&entry(), 31254724, "the archive did not answer in time");
        assert_eq!(document["external"]["submissionId"], 31254724);
        assert_eq!(document["failure"], "the archive did not answer in time");
        assert!(
            document["score"].is_null(),
            "nothing unjudged carries a score"
        );
    }
}
