//! What a `uva@1` problem says about itself.
//!
//! **There is no package.** Decided 2026-08-16: the whole of a UVa problem's
//! configuration is opaque documents the Server already carries.
//!
//! **Two documents, and they answer different questions** (2026-08-22):
//!
//! - the **version's** `props` says *which problem this is* — the archive's
//!   number. Identity, and a fact about the problem rather than about one
//!   activity's use of it, so it is written once at import and every assignment
//!   inherits it. That half is this file's, because the member is spelt `uva`.
//! - the **assignment's** `config` says *how this course judges it* — which
//!   verdicts count as solved, and which languages are allowed. That half is
//!   spelt the same whichever judge holds the problem, so it is read by
//!   `crate::integration::judging` and not here.
//!
//! The language map used to be in there too and is not any more: `uva@1` defines
//! its own six, in `language.rs`. See that file for why holding them per problem
//! was the wrong place.

use serde::Deserialize;

use crate::integration::Setup;

/// The version's document: which problem this is.
#[derive(Debug, Deserialize)]
struct Identity {
    #[serde(rename = "type")]
    kind: Option<String>,
    uva: Option<Uva>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uva {
    problem_number: Option<i64>,
}

/// The problem, out of the two documents that describe it.
///
/// **The number comes from the version and the scoring from the assignment**,
/// and neither substitutes for the other: a problem attached to three courses is
/// one archive number and three ways of counting it.
pub fn read(
    identity: Option<&serde_json::Value>,
    config: Option<&serde_json::Value>,
) -> anyhow::Result<Setup> {
    let identity = identity.ok_or_else(|| {
        anyhow::anyhow!(
            "this problem version carries no props, so there is no UVa problem number \
             to submit to. A problem imported before 2026-08-22 kept its number on the \
             version's `config`, which no longer exists; set `props` to \
             {{\"type\":\"uva@1\",\"uva\":{{\"problemNumber\":N}}}} on the version."
        )
    })?;
    let identity: Identity = serde_json::from_value(identity.clone())
        .map_err(|e| anyhow::anyhow!("the problem version's props cannot be read: {e}"))?;

    if let Some(kind) = &identity.kind {
        if kind != super::PROBLEM_TYPE {
            anyhow::bail!(
                "the problem version's props say type {kind:?}, not {:?}",
                super::PROBLEM_TYPE
            );
        }
    }

    let number = identity
        .uva
        .and_then(|uva| uva.problem_number)
        .ok_or_else(|| anyhow::anyhow!("the problem version's props have no uva.problemNumber"))?;
    if number <= 0 {
        anyhow::bail!("uva.problemNumber is {number}, which is not a problem number");
    }

    let (languages, accepted) = crate::integration::judging(config)?;

    Ok(Setup {
        number,
        accepted,
        languages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **This message had its line breaks collapsed into runs of fourteen
    /// spaces**, and it is what a manager reads when a problem version carries
    /// no number — the one sentence telling them what to set and where.
    ///
    /// Four literals in this repository were in that state. Nothing could catch
    /// it: `rustfmt` does not look inside a string and `clippy` has no opinion
    /// about one, so the guard is a test on the value rather than on the source.
    #[test]
    fn the_message_about_a_missing_number_reads_as_a_sentence() {
        let refused = read(None, None).unwrap_err().to_string();

        assert!(refused.contains("problemNumber"), "{refused}");
        assert!(
            !refused.contains("  "),
            "a run of spaces survived a collapsed line break: {refused}"
        );
    }

    fn document(text: &str) -> Option<serde_json::Value> {
        Some(serde_json::from_str(text).unwrap())
    }

    /// The version's document: which problem this is.
    fn identity() -> Option<serde_json::Value> {
        document(r#"{"type":"uva@1","uva":{"problemNumber":100,"specialJudge":false}}"#)
    }

    #[test]
    fn the_number_comes_from_the_version() {
        let setup = read(identity().as_ref(), None).unwrap();
        assert_eq!(setup.number, 100);
        assert_eq!(setup.accepted, vec!["AC".to_owned()]);
        assert!(setup.languages.is_empty(), "the type defines the six");
    }

    /// The two documents answer different questions, and the assignment's is the
    /// one a course varies: the same imported problem, counted two ways.
    #[test]
    fn the_scoring_comes_from_the_assignment() {
        let lenient = document(r#"{"scoring":{"acceptedVerdicts":["AC","PE"]}}"#);
        let setup = read(identity().as_ref(), lenient.as_ref()).unwrap();

        assert_eq!(setup.accepted, vec!["AC".to_owned(), "PE".to_owned()]);
        assert_eq!(setup.number, 100, "and it says nothing about which problem");
    }

    /// Each of these would otherwise fail somewhere far from its cause.
    #[test]
    fn a_configuration_that_cannot_work_says_which_part() {
        let none = read(None, None).unwrap_err().to_string();
        assert!(none.contains("no UVa problem number"), "{none}");
        // A problem imported before 2026-08-22 kept its number on the version's
        // `config`, which no longer exists. The message has to say so, because
        // the symptom — a job refused before anything leaves — looks identical
        // to a problem nobody configured at all.
        assert!(none.contains("props"), "{none}");

        let no_number = document(r#"{"type":"uva@1"}"#);
        let refused = read(no_number.as_ref(), None).unwrap_err().to_string();
        assert!(refused.contains("uva.problemNumber"), "{refused}");

        let negative = document(r#"{"uva":{"problemNumber":0}}"#);
        let refused = read(negative.as_ref(), None).unwrap_err().to_string();
        assert!(refused.contains("not a problem number"), "{refused}");

        let wrong_type = document(r#"{"type":"standard-io@1","uva":{"problemNumber":100}}"#);
        let refused = read(wrong_type.as_ref(), None).unwrap_err().to_string();
        assert!(refused.contains("uva@1"), "{refused}");
    }
}
