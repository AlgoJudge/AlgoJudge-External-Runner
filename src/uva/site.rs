//! onlinejudge.org: one session, and the submissions made through it.
//!
//! Submitting is an HTML form flow, not an API, so this is the least pleasant
//! module in the repository and the one most likely to be broken by somebody
//! else's layout change. Two habits keep that survivable: the form is found by
//! its **id** rather than by counting tags, and a failure to submit is told
//! apart from a session that lapsed.

use anyhow::{bail, Context};
use scraper::{Html, Selector};

/// Why a submission did not produce an id.
#[derive(Debug)]
pub enum Refused {
    /// The session is gone — re-establish it **once** and try again.
    ///
    /// The proof of concept logged in before every submit, up to fifteen times
    /// in a loop. That is two extra requests per submission against somebody
    /// else's site, and it turns "the password is wrong" into thirty requests
    /// and a plausible ban.
    SessionLapsed,
    /// Anything else. Not retried here: a second attempt at a submission the
    /// site has already refused is a duplicate to a third party.
    Site(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionLapsed => write!(f, "the onlinejudge.org session had lapsed"),
            Self::Site(why) => write!(f, "onlinejudge.org refused the submission: {why}"),
        }
    }
}

/// The hidden fields of the login form.
///
/// **They change per response**, so they are read from the page each time and
/// never cached. Found through `#mod_loginform` with a real parser: the proof of
/// concept split the response on `"<form"` and took element three, which breaks
/// the day anything is added above it.
pub fn hidden_fields(page: &str) -> anyhow::Result<Vec<(String, String)>> {
    let document = Html::parse_document(page);
    let form = Selector::parse("#mod_loginform").expect("a constant selector");
    let hidden = Selector::parse("input[type=hidden]").expect("a constant selector");

    let form = document
        .select(&form)
        .next()
        .context("the login form #mod_loginform is not on the page; the layout changed")?;

    let fields: Vec<_> = form
        .select(&hidden)
        .filter_map(|input| {
            let name = input.value().attr("name")?;
            Some((
                name.to_owned(),
                input.value().attr("value").unwrap_or("").to_owned(),
            ))
        })
        .collect();

    if fields.is_empty() {
        bail!("the login form carries no hidden fields; the layout changed");
    }
    Ok(fields)
}

/// The external submission id, out of the redirect the site answers with.
///
/// **This is the correlation key and there is no other.** Element 0 of a uHunt
/// submission row is this number — confirmed against the live archive on
/// 2026-08-16, where the id in the redirect (`31254724`) was the id uHunt then
/// reported for that submission.
///
/// Both spellings are accepted because the site answers with a URL-encoded
/// message and a reader may or may not have decoded it before getting here.
pub fn sid_from(trail: &str) -> Option<i64> {
    let plus = "Submission+received+with+ID+";
    let spaced = "Submission received with ID ";
    let digits = |at: usize| {
        trail[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    };
    let found = trail
        .find(plus)
        .map(|at| digits(at + plus.len()))
        .or_else(|| trail.find(spaced).map(|at| digits(at + spaced.len())))?;
    found.parse().ok()
}

/// What is sent as the source.
///
/// **Nothing is appended to it.** The specification proposed adding a unique
/// comment line per submission, on a community report that onlinejudge.org
/// refuses code byte-identical to something the same account already sent —
/// which matters here because one robot account means a whole class shares one
/// submission history.
///
/// Measured on 2026-08-16 against the live archive: the same four-line C program
/// was submitted twice from one account, forty-one seconds apart, and **both
/// were judged normally** (sids 31254724 and 31254726, both `WrongAnswer`). The
/// report is not true for that case, so the source travels exactly as its author
/// wrote it.
///
/// The measurement's own limit, said plainly: the duplicated program was
/// *wrong*, so it does not speak for a duplicated **accepted** solution, which
/// is the likelier classroom case. If a duplicate is ever seen refused, this is
/// the function to change and the comment to correct.
pub fn source_to_send(written: &str) -> &str {
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the real page, reduced to what is read from it.
    const LOGIN_PAGE: &str = r#"
        <html><body>
          <form action="/index.php" id="other_form"><input type="hidden" name="decoy" value="no"></form>
          <form action="/index.php?option=com_comprofiler&task=login" id="mod_loginform" method="post">
            <input type="text" name="username" size="18">
            <input type="password" name="passwd" size="18">
            <input type="hidden" name="option" value="com_comprofiler">
            <input type="hidden" name="remember" value="yes">
            <input type="hidden" name="7f1b1a2c3d4e5f60718293a4b5c6d7e8" value="1">
          </form>
        </body></html>"#;

    #[test]
    fn the_form_is_found_by_its_id_not_by_its_position() {
        let fields = hidden_fields(LOGIN_PAGE).unwrap();
        let names: Vec<_> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"option"), "{names:?}");
        assert!(names.contains(&"7f1b1a2c3d4e5f60718293a4b5c6d7e8"), "{names:?}");
        // The decoy sits in an earlier form, which is exactly what a positional
        // reader would have taken.
        assert!(!names.contains(&"decoy"), "a field from another form was read");
    }

    #[test]
    fn a_page_without_the_form_says_the_layout_changed() {
        let refused = hidden_fields("<html><body>signed out</body></html>")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("layout changed"), "{refused}");
    }

    /// The real redirect, captured on 2026-08-16.
    #[test]
    fn the_id_comes_out_of_the_redirect() {
        let encoded = "https://onlinejudge.org/index.php?option=com_onlinejudge&Itemid=25\
                       &page=submit_problem&category=&mosmsg=Submission+received+with+ID+31254724";
        assert_eq!(sid_from(encoded), Some(31254724));

        let decoded = "https://onlinejudge.org/index.php?…&mosmsg=Submission received with ID 31254726";
        assert_eq!(sid_from(decoded), Some(31254726));
    }

    /// While a submission is queued the id is simply absent, which is the
    /// ordinary case and not a parse failure.
    #[test]
    fn no_id_is_none_rather_than_a_panic() {
        assert_eq!(sid_from("https://onlinejudge.org/"), None);
        assert_eq!(sid_from("mosmsg=Submission+received+with+ID+"), None);
    }

    /// The measurement above, pinned: nothing is added to somebody's source.
    #[test]
    fn the_source_travels_exactly_as_written() {
        let written = "#include <stdio.h>\nint main(void){return 0;}\n";
        assert_eq!(source_to_send(written), written);
    }
}
