//! Configuration, in the shape the rest of the product already uses.
//!
//! Prefix `AJ_`, `__` between sections — the same convention the Server and
//! `AlgoJudge-Runner` read, so an operator writing a Compose file does not have
//! to remember which service spells it which way.
//!
//! **Two sections, and the split is the point.** `AJ_Server__*`, `AJ_Runner__*`
//! and `AJ_Lease__*` are this Runner's own and mean the same thing whatever it
//! forwards to. `AJ_External__*` is the judging system it forwards to: which one,
//! where it is, whose account, and how hard to lean on it.
//!
//! **One process serves one external judge.** There is one endpoint and one
//! account here, deliberately — two judges are two deployments of this Runner,
//! each with its own problem types and its own pools, which is how Runners
//! already scale. A single process holding two accounts would need a second
//! copy of every value below and buy nothing.
//!
//! **A wrong number is a refusal, never a quiet default.** Two of the values
//! here are limits on how hard this Runner leans on somebody else's site, and a
//! politeness limit a deployment can silently disable is not a limit.

use anyhow::{bail, Context};

/// Below this the Runner refuses to start.
///
/// Not a default that can be lowered: the poll floor is what stands between this
/// Runner and a request storm against a judge that may publish no rate limit at
/// all — onlinejudge.org does not (searched 2026-08-13, nothing found).
pub const POLL_FLOOR_SECONDS: u64 = 20;

/// The longest lease the Server will grant, whatever is asked for.
///
/// **Not this Runner's choice.** `RunnerService` clamps `leaseSeconds` to
/// `[60, 3600]` on both the claim and the renewal, and `aj-protocol` says so at
/// `ClaimedJob::lease_expires_at`: *the granted deadline is authoritative, and a
/// Runner that renews on its own arithmetic renews on a number the Server never
/// agreed to*. Read off `AlgoJudge-Server` on 2026-08-31.
pub const SERVER_LEASE_CEILING_SECONDS: u32 = 3600;

/// The judge served when nothing says otherwise.
///
/// **The only one there is.** A second is a module beside `crate::uva` and an
/// arm in `main`, not a fork.
pub const DEFAULT_JUDGE: &str = "uva";

/// Where the source cache lives when nothing says otherwise.
///
/// **`AJ_Cache__Path` is the name the sandboxing Runner already reads**, so an
/// operator writing one Compose file for both does not have to remember which
/// of the two spells it which way.
///
/// It was hard-coded in `main` until 2026-08-31, which is half of why the image
/// shipped without the directory: nothing in the repository could name the path,
/// so `.env.example` could not list it and the `Dockerfile` had to agree with a
/// constant it could not see.
pub const DEFAULT_CACHE_PATH: &str = "/var/cache/algojudge-external-runner";

/// How much of the source cache to keep when nothing says otherwise.
///
/// **A fortieth of `AlgoJudge-Runner`'s ten gigabytes, and deliberately.**
/// That Runner caches problem *packages* — test data, reference solutions, a
/// checker — and one of those can be hundreds of megabytes on its own. This one
/// caches nothing but participants' source files, which are text. 256 MiB of
/// text is tens of thousands of submissions.
///
/// Below one submission's worth the cache still works, and simply never hits:
/// `evict_to_fit` skips whatever is being read, so the entry a job is using
/// survives and is evicted once the job lets go. The cost of setting this too
/// low is one download per job, not a failure — which is why nothing refuses a
/// small value.
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Whether the long-poll accelerator is on when nothing says otherwise.
///
/// **Off, and it defaulted to on until 2026-08-31.** The flag has no trigger
/// behind it — the accelerator is accepted and not built — and `schedule` reads
/// it as a promise that verdicts arrive by another route, so it flattens the
/// interval net to its *ceiling*. Every installation that never set the
/// variable was therefore asking the archive once a minute instead of three
/// times, and waiting up to forty seconds longer for each verdict, in exchange
/// for a promptness nothing was delivering.
///
/// A switch defaults to the behaviour that works. When the trigger is built,
/// this becomes a decision again.
pub const DEFAULT_LONG_POLL_ENABLED: bool = false;

#[derive(Debug, Clone)]
pub struct Config {
    pub server_base_url: String,
    pub runner_name: String,
    /// What this Runner declares it serves.
    ///
    /// **Empty means the judge's own**, resolved at registration. An operator
    /// setting `AJ_Runner__ProblemTypes` overrides it, which is the only way to
    /// narrow or widen what a deployment answers for.
    pub problem_types: Vec<String>,

    /// Which pools this Runner belongs to, from `AJ_Runner__Tags`.
    ///
    /// The Server pairs a Runner with work when the two tag lists **share at
    /// least one** entry, and an empty list on either side means `default` — so
    /// naming a pool takes this Runner out of the general queue as surely as it
    /// puts it into a reserved one.
    ///
    /// **Read at the first registration and never again.** Every other field
    /// the Server is told about is refreshed whenever a Runner registers again,
    /// which is how a restart is reported; this one is not, and the operator
    /// owns it in the panel from then on. Changing it here afterwards changes
    /// nothing, deliberately: a restart must not move a Runner into an
    /// examination's pool.
    pub tags: Vec<String>,
    pub key_path: String,

    /// Where a submission's source is cached on its way to the judge.
    ///
    /// **There is no package cache and there is a source cache**, and conflating
    /// the two cost every job in the shipped image until 2026-08-31. An external
    /// problem has no package — its whole configuration travels on the job — so
    /// nothing is ever downloaded for a *problem*. The participant's own file
    /// still is, through the protocol crate's cache, with its checksum verified
    /// before it is read.
    pub cache_path: String,

    /// How much of that cache to keep, from `AJ_Cache__MaxBytes`.
    ///
    /// **It was `256 * 1024 * 1024` written into `main` until 2026-08-31** —
    /// the same shape as the path beside it, which shipped an image that could
    /// not judge anything. A number nobody outside one file can name is a number
    /// nobody can correct.
    pub cache_max_bytes: u64,

    /// Requested at claim time and renewed while a submission is pending.
    ///
    /// **Must exceed `external.pending_timeout`.** The Server's default lease is
    /// ten minutes and its reaper requeues an expired job, so a job left on the
    /// default while this Runner waited fifteen minutes on the judge would be
    /// claimed again and submitted a second time — to somebody else's site.
    pub lease_seconds: u32,

    pub external: External,
}

/// The judging system this Runner forwards to.
#[derive(Debug, Clone)]
pub struct External {
    /// Which integration to run. `uva` is the only one built.
    pub judge: String,

    /// Where the judge is reached for submitting.
    ///
    /// **This and `api_base_url` default to the default judge's own addresses**,
    /// so a deployment of `uva` states neither. They are settings rather than
    /// constants because a test points them at a recorded stand-in, and because
    /// an archive that moves should not need a release.
    pub base_url: String,
    /// Where the judge is read for answers, when that is a different service.
    ///
    /// For UVa it is uHunt: submitting is an HTML form on `onlinejudge.org` and
    /// reading is a JSON API on `uhunt.onlinejudge.org`, which is one judge with
    /// two addresses rather than two judges.
    pub api_base_url: String,

    pub username: String,
    pub password: String,
    /// The judge's numeric id for the account, resolved from the username when
    /// absent.
    pub user_id: Option<u64>,

    pub poll_min: u64,
    pub poll_max: u64,
    pub poll_escalate_after: u64,
    pub submit_min_interval: u64,
    pub pending_timeout: u64,
    pub max_pending: usize,
    pub long_poll_enabled: bool,
}

impl Config {
    pub fn from_environment() -> anyhow::Result<Self> {
        let server_base_url = var("Server__BaseUrl").context(
            "AJ_Server__BaseUrl is required, and must include /api/v1 \
             (for example http://server:8080/api/v1)",
        )?;
        if !server_base_url.contains("/api/") {
            bail!("AJ_Server__BaseUrl is {server_base_url:?}, which has no /api/v1 prefix");
        }

        let config = Self {
            server_base_url,
            runner_name: var("Runner__Name").unwrap_or_else(|_| {
                std::env::var("HOSTNAME").unwrap_or_else(|_| "algojudge-external-runner".into())
            }),
            problem_types: list("Runner__ProblemTypes", ""),
            tags: tags("Runner__Tags"),
            key_path: var("Runner__KeyPath")
                .unwrap_or_else(|_| "/var/lib/algojudge-external-runner/identity.key".into()),
            cache_path: var("Cache__Path").unwrap_or_else(|_| DEFAULT_CACHE_PATH.into()),
            cache_max_bytes: number("Cache__MaxBytes", DEFAULT_CACHE_MAX_BYTES)?,
            lease_seconds: number("Lease__RequestSeconds", 1200)? as u32,

            external: External {
                judge: var("External__Judge").unwrap_or_else(|_| DEFAULT_JUDGE.into()),

                base_url: trailing_slash(
                    &var("External__BaseUrl").unwrap_or_else(|_| "https://onlinejudge.org/".into()),
                ),
                api_base_url: trailing_slash(
                    &var("External__ApiBaseUrl")
                        .unwrap_or_else(|_| "https://uhunt.onlinejudge.org/".into()),
                ),
                username: var("External__Username").context(
                    "AJ_External__Username is required: the account submissions are made under",
                )?,
                // Untrimmed: see `secret`. The username stays trimmed — it is an
                // identifier rather than a secret, and it becomes a path segment
                // in a uHunt request.
                password: secret("External__Password")
                    .context("AJ_External__Password is required")?,
                user_id: match var("External__UserId") {
                    Ok(value) => Some(number_in(&value, "External__UserId")?),
                    Err(_) => None,
                },

                poll_min: number("External__PollMinSeconds", POLL_FLOOR_SECONDS)?,
                poll_max: number("External__PollMaxSeconds", 60)?,
                poll_escalate_after: number("External__PollEscalateAfterSeconds", 120)?,
                submit_min_interval: number("External__SubmitMinIntervalSeconds", 5)?,
                pending_timeout: number("External__PendingTimeoutSeconds", 900)?,
                max_pending: number("External__MaxPending", 20)? as usize,
                long_poll_enabled: flag("External__LongPollEnabled", DEFAULT_LONG_POLL_ENABLED)?,
            },
        };

        config.refuse_what_cannot_work()?;
        Ok(config)
    }

    /// Every way a configuration can be accepted and still be wrong.
    ///
    /// Checked at start-up rather than discovered in an hour: each of these
    /// fails somewhere far from its cause — a lease shorter than the timeout
    /// looks like the judge double-judging, and a poll floor below twenty
    /// looks like nothing at all until somebody else's server complains.
    ///
    /// **This said "the three ways" while there were five**, which is the shape
    /// a count in prose always ends up in. There is no number here now.
    fn refuse_what_cannot_work(&self) -> anyhow::Result<()> {
        if self.external.poll_min < POLL_FLOOR_SECONDS {
            bail!(
                "AJ_External__PollMinSeconds is {}, below the floor of {POLL_FLOOR_SECONDS}. \
                 An external judge may publish no rate limit at all, so this one is not lowered.",
                self.external.poll_min
            );
        }
        if self.external.poll_max < self.external.poll_min {
            bail!(
                "AJ_External__PollMaxSeconds is {}, below AJ_External__PollMinSeconds of {}",
                self.external.poll_max,
                self.external.poll_min
            );
        }
        // **Before the two checks that compute with the lease**, because if the
        // Server is going to clamp it then every number they reason about is
        // one it never agreed to — and the message on the second of them says
        // so out loud ("the Server clamps it at 3600, so this cannot exceed
        // 900") while nothing enforced the antecedent. `tests/lease.rs` has
        // carried the hole in prose since 2026-08-23: 3700 against a pending
        // timeout of 3650 passes every other check, the Server grants 3600, and
        // the job is held fifty seconds past the lease it really has.
        //
        // **No floor to match it**, and that is deliberate: the clamp's lower
        // half grants *more* than was asked, so a lease that is too small is
        // refused below on its own merits and never by being raised.
        if self.lease_seconds > SERVER_LEASE_CEILING_SECONDS {
            bail!(
                "AJ_Lease__RequestSeconds is {}, above the {SERVER_LEASE_CEILING_SECONDS} \
                 seconds the Server will grant. It clamps what it hands out, so this Runner \
                 would renew against a deadline of its own invention and hold a job past the \
                 lease it really has — and the next Runner to claim it would submit the same \
                 solution again.",
                self.lease_seconds
            );
        }
        if u64::from(self.lease_seconds) <= self.external.pending_timeout {
            bail!(
                "AJ_Lease__RequestSeconds is {}, which does not exceed \
                 AJ_External__PendingTimeoutSeconds of {}. The Server would reclaim the job \
                 while this Runner was still waiting on the judge, and the next \
                 Runner to claim it would submit the same solution again.",
                self.lease_seconds,
                self.external.pending_timeout
            );
        }
        // **Renewal rides on this cadence.** A held lease is renewed at the top
        // of the same cycle that asks the judge, so the slowest poll interval
        // is also the slowest renewal. An operator being polite to somebody
        // else's service by raising this — the obvious, well-meant change —
        // stretches the renewal interval with it, and a lease that expires
        // between two renewals is reclaimed, claimed by another Runner, and
        // **the same solution is submitted a second time**. That is the failure
        // this Runner's lease handling exists to prevent, reachable through
        // configuration alone, with nothing in any log to say it happened.
        //
        // Four, matching the keeper in `AlgoJudge-Runner`: three renewals fit
        // inside every lease, so two may fail in a row with the deadline still
        // comfortably ahead.
        if self.external.poll_max.saturating_mul(4) > u64::from(self.lease_seconds) {
            bail!(
                "AJ_External__PollMaxSeconds is {}, which does not fit four times inside \
                 AJ_Lease__RequestSeconds of {}. A lease is renewed on the polling \
                 cycle, so this one could expire between two renewals — and the next \
                 Runner to claim the job would submit the same solution again. \
                 Lower the poll interval, or raise the lease (the Server clamps it \
                 at 3600, so this cannot exceed 900).",
                self.external.poll_max,
                self.lease_seconds
            );
        }
        if self.external.max_pending == 0 {
            bail!("AJ_External__MaxPending is 0, so no job could ever be claimed");
        }
        Ok(())
    }
}

fn var(key: &str) -> Result<String, std::env::VarError> {
    match std::env::var(format!("AJ_{key}")) {
        Ok(value) if value.trim().is_empty() => Err(std::env::VarError::NotPresent),
        Ok(value) => Ok(value.trim().to_owned()),
        Err(e) => Err(e),
    }
}

/// A value read **exactly as it was given**.
///
/// **The trim in `var` is right for a URL and wrong for a credential.** A
/// password with a leading or trailing space is legal on somebody else's site
/// and easy to acquire by pasting one into a `.env`; trimming it sent a
/// different password, the sign-in failed, and until 2026-08-31 that was
/// reported as a lapsed session with two submissions attempted per job and
/// nothing in any log naming the configuration.
///
/// Still absent when it is only whitespace: a password of three spaces is a
/// field somebody left blank, and *required* is a more useful answer than a
/// refusal from the archive.
fn secret(key: &str) -> Result<String, std::env::VarError> {
    match std::env::var(format!("AJ_{key}")) {
        Ok(value) if value.trim().is_empty() => Err(std::env::VarError::NotPresent),
        Ok(value) => Ok(value),
        Err(e) => Err(e),
    }
}

fn list(key: &str, fallback: &str) -> Vec<String> {
    var(key)
        .unwrap_or_else(|_| fallback.to_owned())
        .split(',')
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The pools a Runner belongs to, in the one spelling the Server stores.
///
/// **Lowercased here as well as by the Server**, so the start-up log says what
/// will actually be stored rather than what somebody typed. The Server matches
/// pools by equality, so `Lab-A` here and `lab-a` on an activity would be two
/// pools that read as one — and the failure is a queue that never drains with
/// nothing on any screen to say why.
fn tags(key: &str) -> Vec<String> {
    list(key, "").iter().map(|t| t.to_lowercase()).collect()
}

fn number(key: &str, fallback: u64) -> anyhow::Result<u64> {
    match var(key) {
        Err(_) => Ok(fallback),
        Ok(value) => number_in(&value, key),
    }
}

/// A malformed number names itself.
///
/// An operator who typed a letter O for a zero otherwise gets the default and
/// never learns that their setting did nothing.
fn number_in(value: &str, key: &str) -> anyhow::Result<u64> {
    value
        .parse()
        .with_context(|| format!("AJ_{key} is {value:?}, which is not a number"))
}

fn flag(key: &str, fallback: bool) -> anyhow::Result<bool> {
    match var(key) {
        Err(_) => Ok(fallback),
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            other => bail!("AJ_{key} is {other:?}, which is not true or false"),
        },
    }
}

fn trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_owned()
    } else {
        format!("{url}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            server_base_url: "http://server:8080/api/v1".into(),
            runner_name: "test".into(),
            problem_types: vec![],
            tags: vec![],
            key_path: "/tmp/identity.key".into(),
            cache_path: "/tmp/cache".into(),
            cache_max_bytes: DEFAULT_CACHE_MAX_BYTES,
            lease_seconds: 1200,
            external: External {
                judge: DEFAULT_JUDGE.into(),
                base_url: "https://onlinejudge.org/".into(),
                api_base_url: "https://uhunt.onlinejudge.org/".into(),
                username: "robot".into(),
                password: "secret".into(),
                user_id: None,
                poll_min: 20,
                poll_max: 60,
                poll_escalate_after: 120,
                submit_min_interval: 5,
                pending_timeout: 900,
                max_pending: 20,
                long_poll_enabled: DEFAULT_LONG_POLL_ENABLED,
            },
        }
    }

    /// **One spelling, whoever typed it.** The Server matches pools by equality,
    /// so `Lab-A` here and `lab-a` on an activity would be two pools that read as
    /// one — and the failure is a queue that never drains with nothing on any
    /// screen to say why. The Server normalises what it is sent as well; doing it
    /// here too is what makes the start-up log say what will actually be stored.
    /// **Through `tags()` itself**, not through a copy of what it does. The first
    /// version of this restated the pipeline inline, and would have stayed green
    /// with the lowercasing deleted from the code it was written for.
    #[test]
    fn tags_are_lowercased_trimmed_and_emptied_of_blanks() {
        // Absent is the general pool, which the Server reads as `default`.
        std::env::remove_var("AJ_Runner__Tags");
        assert_eq!(tags("Runner__Tags"), Vec::<String>::new());

        std::env::set_var("AJ_Runner__Tags", "  Lab-A , UVA ");
        assert_eq!(tags("Runner__Tags"), vec!["lab-a", "uva"]);

        std::env::set_var("AJ_Runner__Tags", "lab-a,,lab-b");
        assert_eq!(tags("Runner__Tags"), vec!["lab-a", "lab-b"]);

        std::env::remove_var("AJ_Runner__Tags");
    }

    /// **A credential is not a URL, and one reader trimmed both.**
    ///
    /// A password ending in a space is legal on somebody else's site and is what
    /// pasting into a `.env` produces. Trimming it sent a different password and
    /// the failure surfaced three layers away, as a lapsed session with two
    /// submissions attempted per job.
    #[test]
    fn a_password_is_not_trimmed_and_a_url_is() {
        std::env::set_var("AJ_External__Password", " hunter2 ");
        std::env::set_var("AJ_Server__BaseUrl", "  http://server:8080/api/v1  ");

        assert_eq!(secret("External__Password").unwrap(), " hunter2 ");
        assert_eq!(var("Server__BaseUrl").unwrap(), "http://server:8080/api/v1");

        // Whitespace alone is a field somebody left blank, on either reader.
        std::env::set_var("AJ_External__Password", "   ");
        assert!(secret("External__Password").is_err());

        std::env::remove_var("AJ_External__Password");
        std::env::remove_var("AJ_Server__BaseUrl");
    }

    /// **Two literals in `main` cost this repository an image that could not
    /// judge anything**, and the guard is on the source because no tool can see
    /// it: a hard-coded value compiles, passes `clippy`, and is invisible until
    /// somebody needs to change it on a running installation.
    ///
    /// The cache is built from configuration alone — the path and the ceiling
    /// both — so a number appearing between these parentheses again reddens
    /// this rather than shipping.
    #[test]
    fn main_builds_the_cache_from_configuration_and_not_from_literals() {
        let main = include_str!("main.rs");
        let start = main.find("Cache::new(").expect("main builds a cache");
        let call = &main[start..start + main[start..].find("));").expect("a closed call")];

        assert!(
            call.contains("config.cache_path"),
            "the cache path is not read from configuration: {call}"
        );
        assert!(
            call.contains("config.cache_max_bytes"),
            "the cache ceiling is not read from configuration: {call}"
        );
        assert!(
            !call.chars().any(|c| c.is_ascii_digit()),
            "a literal number survives in the cache construction: {call}"
        );
    }

    /// The ceiling an operator sets is the ceiling the cache gets, and a typo
    /// in it says so rather than silently leaving the default in place.
    #[test]
    fn the_cache_ceiling_is_read_from_the_environment() {
        std::env::remove_var("AJ_Cache__MaxBytes");
        assert_eq!(
            number("Cache__MaxBytes", DEFAULT_CACHE_MAX_BYTES).unwrap(),
            DEFAULT_CACHE_MAX_BYTES
        );

        std::env::set_var("AJ_Cache__MaxBytes", "1073741824");
        assert_eq!(
            number("Cache__MaxBytes", DEFAULT_CACHE_MAX_BYTES).unwrap(),
            1_073_741_824
        );

        std::env::set_var("AJ_Cache__MaxBytes", "256MB");
        let refused = number("Cache__MaxBytes", DEFAULT_CACHE_MAX_BYTES)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("AJ_Cache__MaxBytes"), "{refused}");

        std::env::remove_var("AJ_Cache__MaxBytes");
    }

    /// **`.env.example` claims to list every variable, and nothing checked it.**
    ///
    /// Four were missing when this was written by hand on 2026-08-31, and the
    /// file's own first line promised otherwise. An operator who trusts that
    /// promise cannot configure what it omits: the variable is not undocumented,
    /// it is *invisible*, which is worse because there is nowhere to look.
    ///
    /// Both halves are read as text, because the drift is between two files and
    /// no compiler sees either as configuration. A commented-out line counts as
    /// listed — that is how a switch is offered without being set.
    #[test]
    fn every_variable_the_config_reads_is_in_the_example_and_no_others() {
        let read: std::collections::BTreeSet<String> = include_str!("config.rs")
            .split('"')
            .filter(|piece| a_key(piece))
            .map(|piece| format!("AJ_{piece}"))
            .collect();

        let listed: std::collections::BTreeSet<String> = include_str!("../.env.example")
            .lines()
            .map(|line| line.trim_start_matches('#').trim())
            .filter_map(|line| line.split_once('='))
            .map(|(name, _)| name.trim().to_owned())
            .filter(|name| name.starts_with("AJ_"))
            .collect();

        let missing: Vec<_> = read.difference(&listed).collect();
        assert!(
            missing.is_empty(),
            "read by the config and absent from .env.example: {missing:?}"
        );

        let stale: Vec<_> = listed.difference(&read).collect();
        assert!(
            stale.is_empty(),
            "listed in .env.example and read by nothing: {stale:?}"
        );
    }

    /// A configuration key exactly, and not a sentence that mentions one.
    fn a_key(piece: &str) -> bool {
        let Some((section, rest)) = piece.split_once("__") else {
            return false;
        };
        matches!(
            section,
            "Server" | "Runner" | "Cache" | "External" | "Lease"
        ) && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_alphanumeric())
    }

    #[test]
    fn a_poll_floor_below_twenty_is_refused() {
        let mut config = base();
        config.external.poll_min = 5;
        let refused = config.refuse_what_cannot_work().unwrap_err().to_string();
        assert!(refused.contains("PollMinSeconds"), "{refused}");
        assert!(refused.contains("not lowered"), "{refused}");
    }

    /// **The rule the message beside it already stated and nothing enforced.**
    ///
    /// `tests/lease.rs` has described this hole in prose since 2026-08-23 — a
    /// lease of 3700 against a pending timeout of 3650 clears every other check,
    /// the Server grants 3600, and the job is then held fifty seconds past the
    /// lease it really has. The knowledge lived in a test's doc comment and the
    /// guard lived nowhere.
    /// The default is pinned by what it *does*, not by its own literal.
    ///
    /// `AJ_External__LongPollEnabled` reads as a promise that verdicts arrive
    /// by some route other than asking, so `schedule` flattens the net to its
    /// ceiling. Nothing delivers that promise yet — the trigger is not built —
    /// so an installation that never set the variable polled at the slowest
    /// rate the configuration allows and waited longer for every verdict.
    #[test]
    fn the_accelerator_that_is_not_built_does_not_slow_the_net_down() {
        let min = std::time::Duration::from_secs(20);
        let max = std::time::Duration::from_secs(60);

        assert_eq!(
            crate::schedule::interval(
                DEFAULT_LONG_POLL_ENABLED,
                std::time::Duration::ZERO,
                min,
                max,
                std::time::Duration::from_secs(120),
            ),
            min,
            "a fresh submission is asked about at the floor, not at the ceiling"
        );
    }

    #[test]
    fn a_lease_above_the_servers_ceiling_is_refused() {
        let mut config = base();
        config.lease_seconds = 7200;
        config.external.pending_timeout = 3650;
        let refused = config.refuse_what_cannot_work().unwrap_err().to_string();
        assert!(refused.contains("RequestSeconds"), "{refused}");
        assert!(refused.contains("3600"), "{refused}");

        // The ceiling itself is a setting, not the first refusal — and the
        // parenthetical in the message below it becomes true: nine hundred is
        // exactly what a poll interval may be once the lease is capped here.
        config.lease_seconds = SERVER_LEASE_CEILING_SECONDS;
        config.external.pending_timeout = 900;
        config.external.poll_max = 900;
        config
            .refuse_what_cannot_work()
            .expect("the ceiling the Server grants is a lease this Runner may ask for");
    }

    /// The collision §3.2 of the specification found, refused at start-up rather
    /// than discovered as a double submission to somebody else's site.
    #[test]
    fn a_lease_that_does_not_outlast_the_timeout_is_refused() {
        let mut config = base();
        config.lease_seconds = 600;
        let refused = config.refuse_what_cannot_work().unwrap_err().to_string();
        assert!(
            refused.contains("submit the same solution again"),
            "{refused}"
        );
    }

    /// **The well-meant change that would have cost a double submission.**
    ///
    /// Renewal happens on the polling cycle, so an operator slowing the polling
    /// down to be kind to somebody else's service slows the renewing down with
    /// it. At three hundred seconds against a twenty-minute lease there is still
    /// room; at six hundred there is not, and nothing about the failure would
    /// point here.
    #[test]
    fn a_poll_interval_that_does_not_fit_inside_the_lease_is_refused() {
        let mut config = base();
        config.external.poll_max = 300;
        config
            .refuse_what_cannot_work()
            .expect("four times three hundred fits inside twenty minutes");

        config.external.poll_max = 600;
        let refused = config.refuse_what_cannot_work().unwrap_err().to_string();
        assert!(refused.contains("PollMaxSeconds"), "{refused}");
        assert!(
            refused.contains("submit the same solution again"),
            "{refused}"
        );
    }

    #[test]
    fn the_defaults_are_accepted() {
        base()
            .refuse_what_cannot_work()
            .expect("the defaults must be usable");
    }

    #[test]
    fn a_malformed_number_names_the_key_and_the_value() {
        let refused = number_in("3O", "External__PollMinSeconds")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("AJ_External__PollMinSeconds"), "{refused}");
        assert!(refused.contains("3O"), "{refused}");
    }

    #[test]
    fn a_base_url_is_given_its_trailing_slash() {
        assert_eq!(trailing_slash("https://x.test"), "https://x.test/");
        assert_eq!(trailing_slash("https://x.test/"), "https://x.test/");
    }

    /// **Silence is the judge's own type, not a literal.** An operator who sets
    /// nothing gets whatever the integration serves; the list exists to narrow
    /// or widen that deliberately.
    #[test]
    fn no_problem_types_configured_is_empty_rather_than_a_guess() {
        std::env::remove_var("AJ_Runner__ProblemTypes");
        assert!(list("Runner__ProblemTypes", "").is_empty());
    }
}
