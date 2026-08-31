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
///
/// **Every judge refuses the same two ways**, so the type is
/// `crate::integration::Refused` and is re-exported here for the callers that
/// think of it as this site's answer. `SessionLapsed` is the case the proof of
/// concept got wrong: it logged in before every submit, up to fifteen times in a
/// loop, which is two extra requests per submission against somebody else's site
/// and turns "the password is wrong" into thirty requests and a plausible ban.
pub use crate::integration::Refused;

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

/// Whether this page is still offering the login form.
///
/// **The one signal both directions of the session are read from.** Signed out,
/// `#mod_loginform` is on the page; signed in it is not. `hidden_fields` has
/// found that form by its id since the beginning and for a different reason, so
/// this asks the same question with the same parser rather than by looking for
/// a substring — which is the habit that broke the proof of concept.
pub fn shows_the_login_form(page: &str) -> bool {
    let form = Selector::parse("#mod_loginform").expect("a constant selector");
    Html::parse_document(page).select(&form).next().is_some()
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

// ---------------------------------------------------------------- over the wire

use std::time::{Duration, Instant};

/// onlinejudge.org, with one session held for the life of the process.
///
/// **The mutex is the design, not an implementation detail.** Holding it across
/// a whole submission gives two properties the specification asks for
/// separately: submits are serialised — at most one new row on the account can
/// be ours, which is what makes crash recovery unambiguous — and a lapsed
/// session is re-established once by whoever noticed, rather than by every
/// caller at once, each invalidating the others' cookie.
pub struct Site {
    http: reqwest::Client,
    base: String,
    username: String,
    password: String,
    turn: tokio::sync::Mutex<Turn>,
}

struct Turn {
    signed_in: bool,
    last_submit: Option<Instant>,
}

impl Site {
    pub fn new(base: String, username: String, password: String) -> anyhow::Result<Self> {
        Ok(Self {
            // The cookie jar is the session. It is never written to disk: it is a
            // bearer credential for a third-party account with the same reach as
            // the password, and a restart re-establishing one session is not a
            // cost worth a secret at rest.
            http: reqwest::Client::builder()
                .cookie_store(true)
                .user_agent(concat!(
                    "AlgoJudge-External-Runner/",
                    env!("CARGO_PKG_VERSION"),
                    " (+https://algojudge.app)"
                ))
                .timeout(Duration::from_secs(60))
                .build()?,
            base,
            username,
            password,
            turn: tokio::sync::Mutex::new(Turn {
                signed_in: false,
                last_submit: None,
            }),
        })
    }

    /// Establishes the session. **No credential reaches a log line here.**
    ///
    /// **A 200 is not a session**, and until 2026-08-31 that was the whole of
    /// the check. onlinejudge.org answers a refused sign-in with 200 and the
    /// login page again, so the status said only that a web server answered —
    /// and the caller set `signed_in = true` on it. A wrong password therefore
    /// produced a submission POST that landed back on the login form, a retry,
    /// a second sign-in and a second submission, for **every job, for ever**,
    /// reported as a lapsed session and never as a credential. That is the
    /// "thirty requests and a plausible ban" this module's own header claims to
    /// have been designed against.
    ///
    /// The fixtures had modelled the difference since they were written and
    /// nothing read them: the login stand-in answers a page carrying `logout`,
    /// and `SIGNED_OUT` in `tests/archive.rs` is `#mod_loginform`.
    async fn sign_in(&self) -> anyhow::Result<()> {
        let page = self.http.get(&self.base).send().await?.text().await?;
        let mut form = hidden_fields(&page)?;
        form.push(("username".into(), self.username.clone()));
        form.push(("passwd".into(), self.password.clone()));
        form.push(("remember".into(), "yes".into()));
        form.push(("Submit".into(), "Login".into()));

        let answer = self
            .http
            .post(format!(
                "{}index.php?option=com_comprofiler&task=login",
                self.base
            ))
            .header(reqwest::header::REFERER, &self.base)
            .form(&form)
            .send()
            .await?;

        if !answer.status().is_success() {
            anyhow::bail!(
                "onlinejudge.org answered {} to the sign-in",
                answer.status()
            );
        }

        let page = answer.text().await?;
        if shows_the_login_form(&page) {
            bail!(
                "onlinejudge.org answered the sign-in with the login form again, \
                 which is what it does when the credentials are refused"
            );
        }
        // **Both signals, and both hard** (decided 2026-08-31). The form's
        // absence is unambiguous and carries the defect; the word is a string on
        // somebody else's page, so requiring it means this Runner stops working
        // the day onlinejudge.org retitles that link — and no fixture here can
        // predict that day. The trade was taken with that known: a session
        // wrongly believed in costs submissions to a third party's account, and
        // refusing to start is the cheaper failure of the two.
        if !page.to_ascii_lowercase().contains("logout") {
            bail!(
                "onlinejudge.org's answer to the sign-in carries no logout link, so \
                 nothing on it says a session was established"
            );
        }
        Ok(())
    }

    /// Sends one submission and returns the archive's id for it.
    ///
    /// Exactly one re-login and one retry. The proof of concept looped fifteen
    /// times, which turns a wrong password into thirty requests and a plausible
    /// ban.
    pub async fn submit(
        &self,
        problem_number: i64,
        language_id: i64,
        source: &str,
        min_interval: Duration,
    ) -> Result<i64, Refused> {
        let mut turn = self.turn.lock().await;

        if let Some(last) = turn.last_submit {
            let since = last.elapsed();
            if since < min_interval {
                tokio::time::sleep(min_interval - since).await;
            }
        }

        // **Twice, written twice.** This was a loop with a bound of two and a
        // guard inside it that returned on the second pass — so the bound
        // enforced nothing, and a sabotage that raised it to fifteen changed no
        // behaviour and reddened no test. Straight-line, the rule is where a
        // reader looks for it and a third attempt cannot be added by accident.
        if let Some(sid) = self
            .attempt(&mut turn, problem_number, language_id, source)
            .await?
        {
            return Ok(sid);
        }

        tracing::warn!("no submission id came back; re-establishing the session once");
        turn.signed_in = false;
        self.attempt(&mut turn, problem_number, language_id, source)
            .await?
            .ok_or(Refused::SessionLapsed)
    }

    /// One sign-in if needed, and one submission.
    async fn attempt(
        &self,
        turn: &mut Turn,
        problem_number: i64,
        language_id: i64,
        source: &str,
    ) -> Result<Option<i64>, Refused> {
        if !turn.signed_in {
            self.sign_in()
                .await
                .map_err(|e| Refused::Site(e.to_string()))?;
            turn.signed_in = true;
        }
        let sent = self
            .send(problem_number, language_id, source)
            .await
            .map_err(|e| Refused::Site(e.to_string()))?;
        turn.last_submit = Some(Instant::now());
        Ok(sent)
    }

    async fn send(
        &self,
        problem_number: i64,
        language_id: i64,
        source: &str,
    ) -> anyhow::Result<Option<i64>> {
        let answer = self
            .http
            .post(format!(
                "{}index.php?option=com_onlinejudge&Itemid=25&page=save_submission",
                self.base
            ))
            .header(
                reqwest::header::REFERER,
                format!(
                    "{}index.php?option=com_onlinejudge&Itemid=25&page=submit_problem",
                    self.base
                ),
            )
            .form(&[
                ("problemid", ""),
                ("category", ""),
                ("codeupl", ""),
                ("localid", &problem_number.to_string()),
                ("language", &language_id.to_string()),
                ("code", source_to_send(source)),
            ])
            .send()
            .await?;

        // The id is in the address the redirect chain ended at, which reqwest
        // has already followed. Confirmed against the live archive 2026-08-16.
        Ok(sid_from(answer.url().as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Both directions of the session are read from one page**, with the
    /// parser rather than a substring: `logout` appears in the prose of a
    /// signed-in page and could appear in the prose of a signed-out one.
    #[test]
    fn the_login_form_is_how_a_signed_out_page_is_known() {
        assert!(shows_the_login_form(LOGIN_PAGE));
        assert!(!shows_the_login_form(
            "<html><body>welcome, robot — <a href=\"/logout\">logout</a></body></html>"
        ));
        assert!(!shows_the_login_form(
            "<html><body>nothing here</body></html>"
        ));
    }

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
        assert!(
            names.contains(&"7f1b1a2c3d4e5f60718293a4b5c6d7e8"),
            "{names:?}"
        );
        // The decoy sits in an earlier form, which is exactly what a positional
        // reader would have taken.
        assert!(
            !names.contains(&"decoy"),
            "a field from another form was read"
        );
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

        let decoded =
            "https://onlinejudge.org/index.php?…&mosmsg=Submission received with ID 31254726";
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
