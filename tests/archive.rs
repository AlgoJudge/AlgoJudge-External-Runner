//! The archive, recorded.
//!
//! **The live archive is never a test dependency.** It is somebody else's
//! infrastructure, it publishes no rate limit, and a suite that hammered it on
//! every commit would be exactly the behaviour this Runner is written to avoid.
//! Everything here runs against a stand-in that answers what the real one
//! answered on 2026-08-16.
//!
//! The seam is the base URL, which production code takes as a field: nothing
//! under test knows it is talking to a fake.

use std::time::Duration;

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The login page, reduced to what is read from it — including a decoy form
/// above the real one, because that is what a positional reader gets wrong.
const LOGIN_PAGE: &str = r#"<html><body>
  <form id="search_form"><input type="hidden" name="decoy" value="no"></form>
  <form id="mod_loginform" method="post" action="/index.php?option=com_comprofiler&task=login">
    <input type="text" name="username">
    <input type="password" name="passwd">
    <input type="hidden" name="option" value="com_comprofiler">
    <input type="hidden" name="7f1b1a2c3d4e5f60718293a4b5c6d7e8" value="1">
  </form>
</body></html>"#;

/// What a signed-out submission lands back on: the login page, and no id.
const SIGNED_OUT: &str = "<html><body><form id=\"mod_loginform\"><input type=\"hidden\" name=\"option\" value=\"x\"></form></body></html>";

async fn site_answering(server: &MockServer, submission: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOGIN_PAGE))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/index.php"))
        .and(query_param("task", "login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>logout</html>"))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/index.php"))
        .and(query_param("page", "save_submission"))
        .respond_with(submission)
        .mount(server)
        .await;
}

fn site(base: &str) -> algojudge_external_runner::uva::site::Site {
    algojudge_external_runner::uva::site::Site::new(
        format!("{base}/"),
        "robot".into(),
        "not-a-real-password".into(),
    )
    .expect("the client builds")
}

/// The whole submit flow, ending where the id actually lives.
#[tokio::test]
async fn a_submission_comes_back_with_the_archive_s_own_id() {
    let server = MockServer::start().await;
    let landing = format!(
        "{}/index.php?option=com_onlinejudge&Itemid=25&page=submit_problem\
         &category=&mosmsg=Submission+received+with+ID+31254724",
        server.uri()
    );
    site_answering(
        &server,
        ResponseTemplate::new(302).insert_header("Location", landing.as_str()),
    )
    .await;
    // The address the redirect lands on has to answer something.
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let sid = site(&server.uri())
        .submit(100, 1, "int main(){}\n", Duration::from_millis(0))
        .await
        .expect("the submission is accepted");

    assert_eq!(sid, 31254724);
}

/// A lapsed session is re-established **once**, and then given up on.
///
/// The proof of concept looped fifteen times, which turns a wrong password into
/// thirty requests against somebody else's site and a plausible ban.
#[tokio::test]
async fn a_lapsed_session_is_re_established_once_and_not_fifteen_times() {
    let server = MockServer::start().await;
    site_answering(
        &server,
        ResponseTemplate::new(200).set_body_string(SIGNED_OUT),
    )
    .await;

    let refused = site(&server.uri())
        .submit(100, 1, "int main(){}\n", Duration::from_millis(0))
        .await
        .expect_err("no id came back, so this cannot be a success");
    assert!(
        matches!(
            refused,
            algojudge_external_runner::uva::site::Refused::SessionLapsed
        ),
        "{refused}"
    );

    // Two submissions and two sign-ins: the first try, and exactly one retry.
    let sent = server.received_requests().await.unwrap();
    let submits = sent
        .iter()
        .filter(|r| r.url.query().is_some_and(|q| q.contains("save_submission")))
        .count();
    let logins = sent
        .iter()
        .filter(|r| r.url.query().is_some_and(|q| q.contains("task=login")))
        .count();
    assert_eq!(submits, 2, "one attempt and one retry, no more");
    assert_eq!(logins, 2, "one sign-in each, not a loop of fifteen");
}

/// Serialisation is a correctness requirement, not politeness: with one submit
/// in flight at a time, at most one new row on the account can be ours.
#[tokio::test]
async fn two_submissions_do_not_overlap_and_keep_their_distance() {
    let server = MockServer::start().await;
    let landing = format!(
        "{}/index.php?mosmsg=Submission+received+with+ID+31254724",
        server.uri()
    );
    site_answering(
        &server,
        ResponseTemplate::new(302).insert_header("Location", landing.as_str()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/index.php"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let site = site(&server.uri());
    let started = std::time::Instant::now();
    site.submit(100, 1, "a\n", Duration::from_millis(0))
        .await
        .unwrap();
    site.submit(100, 1, "b\n", Duration::from_millis(400))
        .await
        .unwrap();

    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "the second submission did not wait its interval ({:?})",
        started.elapsed()
    );
}

// ------------------------------------------------------------------------ uHunt

fn uhunt(base: &str) -> algojudge_external_runner::uva::uhunt::Uhunt {
    algojudge_external_runner::uva::uhunt::Uhunt::new(reqwest::Client::new(), format!("{base}/"))
}

/// The real answer, captured 2026-08-16.
#[tokio::test]
async fn a_problem_is_looked_up_by_its_public_number() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/p/num/100"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"pid":36,"num":100,"title":"The 3n + 1 problem","dacu":109011,"mrun":0,
                "mmem":1000000000,"nover":0,"sube":6949,"noj":0,"inq":0,"ce":139243,"rf":0,
                "re":100139,"ole":387,"tle":78877,"mle":5209,"wa":361824,"pe":6555,
                "ac":256349,"rtl":3000,"status":1,"rej":0}"#,
        ))
        .mount(&server)
        .await;

    let problem = uhunt(&server.uri()).problem(100).await.unwrap();
    assert_eq!(problem.pid, 36, "the id a submission row carries");
    assert_eq!(problem.status, 1);
}

/// A window of the shared account, holding one of ours and two strangers.
#[tokio::test]
async fn the_window_carries_submissions_that_are_not_ours() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/subs-user/1064989/31254723"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"name":"A Robot","uname":"robot","subs":[
                [31254724,36,70,0,1786854437,1,-1],
                [31254725,4838,90,60,1786854450,5,520],
                [31254726,36,70,0,1786854478,1,-1]]}"#,
        ))
        .mount(&server)
        .await;

    let rows = uhunt(&server.uri()).since(1064989, 31254723).await.unwrap();
    assert_eq!(rows.len(), 3, "everything in the window is returned");
    assert_eq!(rows[0].sid, 31254724);
    assert_eq!(rows[0].verdict_id, 70);
    // Which of them are ours is `Pending::matched`'s question, not this one's.
}

/// A number the archive does not have is refused by name rather than guessed at.
#[tokio::test]
async fn an_unknown_problem_number_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/p/num/999999"))
        .respond_with(ResponseTemplate::new(200).set_body_string("null"))
        .mount(&server)
        .await;

    let refused = uhunt(&server.uri())
        .problem(999999)
        .await
        .expect_err("there is no such problem")
        .to_string();
    assert!(refused.contains("999999"), "{refused}");
}
