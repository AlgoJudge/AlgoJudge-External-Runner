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
//!   inherits it.
//! - the **assignment's** `config` says *how this course judges it* — which
//!   verdicts count as solved. A course can accept a presentation error where a
//!   contest counts only an accepted answer, on the **same imported problem**,
//!   without a second copy of it and without anything new on the Server.
//!
//! The language map used to be in there too and is not any more: `uva@1` defines
//! its own six, in `language.rs`. See that file for why holding them per problem
//! was the wrong place.

use serde::Deserialize;

/// The problem, as this Runner needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// The public number — what a person types and what `localid` is set to.
    pub number: i64,
    /// Which verdicts count as solved. Empty is not allowed: a problem nobody
    /// can pass is a configuration mistake, not a strict activity.
    pub accepted: Vec<String>,
}

/// The version's document: which problem this is.
#[derive(Debug, Deserialize)]
struct Identity {
    #[serde(rename = "type")]
    kind: Option<String>,
    uva: Option<Uva>,
}

/// The assignment's document: how this course judges it.
#[derive(Debug, Deserialize)]
struct Judging {
    scoring: Option<Scoring>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uva {
    problem_number: Option<i64>,
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
            "this problem version carries no props, so there is no UVa problem number to              submit to. A problem imported before 2026-08-22 kept its number on the version's              `config`, which no longer exists; set `props` to              {{\"type\":\"uva@1\",\"uva\":{{\"problemNumber\":N}}}} on the version."
        )
    })?;
    let identity: Identity = serde_json::from_value(identity.clone())
        .map_err(|e| anyhow::anyhow!("the problem version's props cannot be read: {e}"))?;

    if let Some(kind) = &identity.kind {
        if kind != "uva@1" {
            anyhow::bail!("the problem version's props say type {kind:?}, not \"uva@1\"");
        }
    }

    let number = identity
        .uva
        .and_then(|uva| uva.problem_number)
        .ok_or_else(|| anyhow::anyhow!("the problem version's props have no uva.problemNumber"))?;
    if number <= 0 {
        anyhow::bail!("uva.problemNumber is {number}, which is not a problem number");
    }

    // **Absent is allowed here and was not before.** An assignment that says
    // nothing about scoring gets the strict default, which is the answer a
    // course would have written anyway; refusing would make every attachment
    // carry a document to say "as usual".
    let accepted = match config
        .map(|c| serde_json::from_value::<Judging>(c.clone()))
        .transpose()
        .map_err(|e| anyhow::anyhow!("the assignment's configuration cannot be read: {e}"))?
        .and_then(|j| j.scoring)
    {
        Some(scoring) if !scoring.accepted_verdicts.is_empty() => scoring.accepted_verdicts,
        _ => vec![STRICT.to_owned()],
    };

    Ok(Setup { number, accepted })
}

impl Setup {
    /// The archive's number for a language, or a refusal naming what is on offer.
    ///
    /// Refused here as well as in the Client, because a rule only the Client
    /// applies is a rule a devtools console turns off.
    ///
    /// **Against the type's own catalogue**, not against something the problem
    /// carried: `uva@1` offers what onlinejudge.org offers, which is the same
    /// six for every problem in the archive.
    pub fn language(&self, wanted: Option<&str>) -> anyhow::Result<i64> {
        let wanted = wanted.ok_or_else(|| {
            anyhow::anyhow!("the submission names no language, and UVa needs one")
        })?;
        crate::language::for_id(wanted)
            .map(|l| l.number)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "onlinejudge.org does not accept {wanted:?}; it accepts {:?}",
                    crate::language::ids()
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Option<serde_json::Value> {
        Some(serde_json::from_str(text).unwrap())
    }

    /// The version's document: which problem this is.
    fn identity() -> Option<serde_json::Value> {
        document(r#"{"type":"uva@1","uva":{"problemNumber":100,"specialJudge":false}}"#)
    }

    #[test]
    fn the_number_comes_from_the_version_and_the_languages_from_the_type() {
        let setup = read(identity().as_ref(), None).unwrap();
        assert_eq!(setup.number, 100);
        assert_eq!(setup.accepted, vec!["AC".to_owned()]);

        // **Not out of either document.** `uva@1` offers what onlinejudge.org
        // offers, which is the same six for every problem in the archive —
        // holding them per problem meant writing six numbers into every import,
        // and an import that wrote none produced a problem nobody could submit
        // to.
        assert_eq!(setup.language(Some("cpp11-gcc")).unwrap(), 5);
    }

    /// The two documents answer different questions, and the assignment's is the
    /// one a course varies: the same imported problem, counted two ways.
    #[test]
    fn an_assignment_may_widen_what_counts_as_solved() {
        let lenient = document(r#"{"scoring":{"acceptedVerdicts":["AC","PE"]}}"#);
        let setup = read(identity().as_ref(), lenient.as_ref()).unwrap();

        assert_eq!(setup.accepted, vec!["AC".to_owned(), "PE".to_owned()]);
        assert_eq!(setup.number, 100, "and it says nothing about which problem");
    }

    /// **Absent is allowed, and was not before.** An assignment that says nothing
    /// about scoring gets the strict default, which is what a course would have
    /// written anyway; refusing would make every attachment carry a document to
    /// say "as usual".
    #[test]
    fn strict_is_the_default_when_the_assignment_says_nothing() {
        assert_eq!(
            read(identity().as_ref(), None).unwrap().accepted,
            vec!["AC".to_owned()],
        );
        let bare = document(r#"{}"#);
        assert_eq!(
            read(identity().as_ref(), bare.as_ref()).unwrap().accepted,
            vec!["AC".to_owned()],
        );
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

    /// A language the archive does not offer is refused by name, with the list.
    #[test]
    fn an_unlisted_language_is_refused_and_says_what_is_on_offer() {
        let setup = read(identity().as_ref(), None).unwrap();
        let refused = setup.language(Some("rust")).unwrap_err().to_string();
        assert!(refused.contains("rust"), "{refused}");
        assert!(refused.contains("cpp11-gcc"), "{refused}");

        let missing = setup.language(None).unwrap_err().to_string();
        assert!(missing.contains("names no language"), "{missing}");
    }
}
