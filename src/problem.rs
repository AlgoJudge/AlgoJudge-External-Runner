//! What a `uva@1` problem says about itself.
//!
//! **There is no package.** Decided 2026-08-16: the whole of a UVa problem's
//! configuration is the opaque document the Server already carries, merged from
//! the problem version and the activity's assignment — `ProblemVersion.Config`
//! then `SeriesProblem.Config`, the later winning member by member, merged by a
//! Server that reads neither.
//!
//! That merge is not a detail here, it is the feature: a course can accept a
//! presentation error where a contest counts only an accepted answer, on the
//! **same imported problem**, without a second copy of it and without anything
//! new on the Server.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The problem, as this Runner needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// The public number — what a person types and what `localid` is set to.
    pub number: i64,
    /// AlgoJudge's language name to the archive's own id.
    ///
    /// Held here rather than compiled in, so a language UVa adds is a
    /// re-published problem and not a release of this Runner.
    pub languages: BTreeMap<String, i64>,
    /// Which verdicts count as solved. Empty is not allowed: a problem nobody
    /// can pass is a configuration mistake, not a strict activity.
    pub accepted: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Document {
    format: Option<String>,
    uva: Option<Uva>,
    #[serde(default)]
    languages: BTreeMap<String, i64>,
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

pub fn read(config: Option<&serde_json::Value>) -> anyhow::Result<Setup> {
    let config = config.ok_or_else(|| {
        anyhow::anyhow!(
            "this problem carries no configuration, so there is no UVa problem number to submit to"
        )
    })?;
    let document: Document = serde_json::from_value(config.clone())
        .map_err(|e| anyhow::anyhow!("the problem's configuration cannot be read: {e}"))?;

    if let Some(format) = &document.format {
        if format != "uva" {
            anyhow::bail!("the problem's configuration says format {format:?}, not \"uva\"");
        }
    }

    let number = document
        .uva
        .and_then(|uva| uva.problem_number)
        .ok_or_else(|| anyhow::anyhow!("the problem's configuration has no uva.problemNumber"))?;
    if number <= 0 {
        anyhow::bail!("uva.problemNumber is {number}, which is not a problem number");
    }

    if document.languages.is_empty() {
        anyhow::bail!("the problem's configuration lists no languages, so nothing may be sent");
    }

    let accepted = match document.scoring {
        Some(scoring) if !scoring.accepted_verdicts.is_empty() => scoring.accepted_verdicts,
        _ => vec![STRICT.to_owned()],
    };

    Ok(Setup {
        number,
        languages: document.languages,
        accepted,
    })
}

impl Setup {
    /// The archive's id for a language, or a refusal naming what is on offer.
    ///
    /// Refused here as well as in the Client, because a rule only the Client
    /// applies is a rule a devtools console turns off.
    pub fn language(&self, wanted: Option<&str>) -> anyhow::Result<i64> {
        let wanted = wanted.ok_or_else(|| {
            anyhow::anyhow!("the submission names no language, and UVa needs one")
        })?;
        self.languages.get(wanted).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "this problem does not accept {wanted:?} on onlinejudge.org; it accepts {:?}",
                self.languages.keys().collect::<Vec<_>>()
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

    fn full() -> Option<serde_json::Value> {
        document(
            r#"{"format":"uva","version":1,
                "uva":{"problemNumber":100,"specialJudge":false},
                "languages":{"c":1,"cpp":3,"cpp11":5,"java":2,"pascal":4,"python":6},
                "scoring":{"maxScore":1,"acceptedVerdicts":["AC"]}}"#,
        )
    }

    #[test]
    fn the_number_and_the_languages_come_out_of_the_configuration() {
        let setup = read(full().as_ref()).unwrap();
        assert_eq!(setup.number, 100);
        assert_eq!(setup.language(Some("cpp11")).unwrap(), 5);
        assert_eq!(setup.accepted, vec!["AC".to_owned()]);
    }

    /// The merge the Server already does, seen from here: the activity widened
    /// what counts as solved, and nothing else changed.
    #[test]
    fn an_activity_may_widen_what_counts_as_solved() {
        let lenient = document(
            r#"{"format":"uva","uva":{"problemNumber":100},
                "languages":{"c":1},
                "scoring":{"acceptedVerdicts":["AC","PE"]}}"#,
        );
        let setup = read(lenient.as_ref()).unwrap();
        assert_eq!(setup.accepted, vec!["AC".to_owned(), "PE".to_owned()]);
    }

    #[test]
    fn strict_is_the_default_when_nothing_says_otherwise() {
        let bare = document(r#"{"uva":{"problemNumber":100},"languages":{"c":1}}"#);
        assert_eq!(read(bare.as_ref()).unwrap().accepted, vec!["AC".to_owned()]);
    }

    /// Each of these would otherwise fail somewhere far from its cause.
    #[test]
    fn a_configuration_that_cannot_work_says_which_part() {
        let no_config = read(None).unwrap_err().to_string();
        assert!(no_config.contains("no UVa problem number"), "{no_config}");

        let no_number = document(r#"{"languages":{"c":1}}"#);
        let refused = read(no_number.as_ref()).unwrap_err().to_string();
        assert!(refused.contains("uva.problemNumber"), "{refused}");

        let no_languages = document(r#"{"uva":{"problemNumber":100}}"#);
        let refused = read(no_languages.as_ref()).unwrap_err().to_string();
        assert!(refused.contains("no languages"), "{refused}");

        let wrong_format = document(r#"{"format":"standard-io","uva":{"problemNumber":100},"languages":{"c":1}}"#);
        let refused = read(wrong_format.as_ref()).unwrap_err().to_string();
        assert!(refused.contains("not \"uva\""), "{refused}");
    }

    /// A language the problem does not offer is refused by name, with the list.
    #[test]
    fn an_unlisted_language_is_refused_and_says_what_is_on_offer() {
        let setup = read(full().as_ref()).unwrap();
        let refused = setup.language(Some("rust")).unwrap_err().to_string();
        assert!(refused.contains("rust"), "{refused}");
        assert!(refused.contains("cpp11"), "{refused}");

        let missing = setup.language(None).unwrap_err().to_string();
        assert!(missing.contains("names no language"), "{missing}");
    }
}
