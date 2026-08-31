//! The six languages onlinejudge.org offers, and what to call each of them.
//!
//! **The problem type defines these, not a problem's configuration** (decided
//! 2026-08-22). It read them out of the configuration document until then, on
//! the argument that a language the archive adds should be a re-published
//! problem rather than a release of this Runner. What that argument missed is
//! that the list is a property of *the archive*, which every `uva@1` problem
//! shares — so holding it per problem meant writing the same six numbers into
//! every import, and an import that wrote none produced a problem nobody could
//! submit to. That failure was found in a live run on 2026-08-16 and is what
//! this closes.
//!
//! A language the archive adds is now a release of this Runner, which is one
//! release for all of them rather than an edit to every problem ever imported.
//!
//! ## The labels are not `standard-io@1`'s
//!
//! Three ids are shared with it — `c89-gcc`, `cpp11-gcc`, `python3` — because
//! they mean the same language, and sharing them lets one screen resolve a
//! label whichever type produced a submission. **The labels differ, and must**:
//! the compilers here are the archive's, pinned at the archive's versions.
//! `cpp11-gcc` in `standard-io@1` is GCC 14 with our flags; here it is GCC 5.3.0
//! with UVa's. Showing a participant "C++11 (GCC)" in both places would say the
//! two were judged by the same compiler.

use crate::integration::Language;

/// The six, in the order the archive's own form lists them.
///
/// **Only `5` has been watched work.** A real submission was accepted under it
/// on 2026-08-16 (sid 31255986). The other five are read off the archive's form
/// and nobody here has submitted through them; that distinction is recorded in
/// the README and is the reason this table is small enough to check by hand.
pub const CATALOGUE: &[Language] = &[
    Language {
        id: "c89-gcc",
        label: "C89 / ANSI C (GCC 5.3.0)",
        number: 1,
    },
    Language {
        id: "java8",
        label: "Java 8 (OpenJDK 1.8.0)",
        number: 2,
    },
    Language {
        id: "cpp98-gcc",
        label: "C++98 (GCC 5.3.0)",
        number: 3,
    },
    Language {
        id: "pascal-fpc",
        label: "Pascal (Free Pascal 3.0.0)",
        number: 4,
    },
    Language {
        id: "cpp11-gcc",
        label: "C++11 (GCC 5.3.0)",
        number: 5,
    },
    Language {
        id: "python3",
        label: "Python 3 (CPython 3.5.1)",
        number: 6,
    },
];

pub fn for_id(id: &str) -> Option<&'static Language> {
    CATALOGUE.iter().find(|l| l.id == id)
}

/// Every id, for a refusal that says what is on offer.
pub fn ids() -> Vec<&'static str> {
    CATALOGUE.iter().map(|l| l.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_the_six_the_archive_offers() {
        assert_eq!(
            ids(),
            vec![
                "c89-gcc",
                "java8",
                "cpp98-gcc",
                "pascal-fpc",
                "cpp11-gcc",
                "python3"
            ],
        );
    }

    /// The one number a real submission has been accepted under.
    #[test]
    fn the_measured_entry_is_the_one_that_was_measured() {
        assert_eq!(for_id("cpp11-gcc").unwrap().number, 5);
    }

    /// Two Runners must not send the same submission to two different
    /// compilers, so no number may appear twice.
    #[test]
    fn no_two_languages_post_the_same_value() {
        let mut numbers: Vec<i64> = CATALOGUE.iter().map(|l| l.number).collect();
        numbers.sort_unstable();
        let before = numbers.len();
        numbers.dedup();
        assert_eq!(numbers.len(), before, "two entries share an archive number");
    }

    /// **The labels are deliberately not `standard-io@1`'s.** The three shared
    /// ids mean the same language and are judged by different compilers, so a
    /// label that did not say which would be a claim about how a submission was
    /// built.
    #[test]
    fn a_shared_id_still_names_the_archives_own_compiler() {
        for id in ["c89-gcc", "cpp11-gcc", "python3"] {
            let label = for_id(id).unwrap().label;
            assert!(
                label.contains("5.3.0") || label.contains("3.5.1"),
                "{id} is labelled {label:?}, which does not say whose compiler it is",
            );
        }
    }

    #[test]
    fn a_language_the_archive_does_not_offer_is_not_in_the_catalogue() {
        assert!(for_id("cpp20-gcc").is_none());
        assert!(for_id("pypy3").is_none());
        assert!(for_id("").is_none());
    }
}
