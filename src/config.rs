//! Configuration, in the shape the rest of the product already uses.
//!
//! Prefix `AJ_`, `__` between sections — the same convention the Server and
//! `AlgoJudge-Runner` read, so an operator writing a Compose file does not have
//! to remember which service spells it which way.
//!
//! **A wrong number is a refusal, never a quiet default.** Two of the values
//! here are limits on how hard this Runner leans on somebody else's site, and a
//! politeness limit a deployment can silently disable is not a limit.

use anyhow::{bail, Context};

/// Below this the Runner refuses to start.
///
/// Not a default that can be lowered: the poll floor is what stands between this
/// module and a request storm against an archive that publishes no rate limit at
/// all (searched 2026-08-13, nothing found).
pub const POLL_FLOOR_SECONDS: u64 = 20;

#[derive(Debug, Clone)]
pub struct Config {
    pub server_base_url: String,
    pub runner_name: String,
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

    pub uva_base_url: String,
    pub uhunt_base_url: String,
    pub uva_username: String,
    pub uva_password: String,
    /// Resolved from the username through `uname2uid` when absent.
    pub uva_user_id: Option<u64>,

    pub poll_min: u64,
    pub poll_max: u64,
    pub poll_escalate_after: u64,
    pub submit_min_interval: u64,
    pub pending_timeout: u64,
    pub max_pending: usize,
    pub long_poll_enabled: bool,

    /// Requested at claim time and renewed while a submission is pending.
    ///
    /// **Must exceed `pending_timeout`.** The Server's default lease is ten
    /// minutes and its reaper requeues an expired job, so a job left on the
    /// default while this Runner waited fifteen minutes on the archive would be
    /// claimed again and submitted a second time — to somebody else's site.
    pub lease_seconds: u32,
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
                std::env::var("HOSTNAME").unwrap_or_else(|_| "algojudge-runner-uva".into())
            }),
            problem_types: list("Runner__ProblemTypes", "uva@1"),
            tags: tags("Runner__Tags"),
            key_path: var("Runner__KeyPath")
                .unwrap_or_else(|_| "/var/lib/algojudge-runner-uva/identity.key".into()),

            uva_base_url: trailing_slash(
                &var("Uva__BaseUrl").unwrap_or_else(|_| "https://onlinejudge.org/".into()),
            ),
            uhunt_base_url: trailing_slash(
                &var("Uva__UhuntBaseUrl")
                    .unwrap_or_else(|_| "https://uhunt.onlinejudge.org/".into()),
            ),
            uva_username: var("Uva__Username")
                .context("AJ_Uva__Username is required: the account submissions are made under")?,
            uva_password: var("Uva__Password").context("AJ_Uva__Password is required")?,
            uva_user_id: match var("Uva__UserId") {
                Ok(value) => Some(number_in(&value, "Uva__UserId")?),
                Err(_) => None,
            },

            poll_min: number("Uva__PollMinSeconds", POLL_FLOOR_SECONDS)?,
            poll_max: number("Uva__PollMaxSeconds", 60)?,
            poll_escalate_after: number("Uva__PollEscalateAfterSeconds", 120)?,
            submit_min_interval: number("Uva__SubmitMinIntervalSeconds", 5)?,
            pending_timeout: number("Uva__PendingTimeoutSeconds", 900)?,
            max_pending: number("Uva__MaxPending", 20)? as usize,
            long_poll_enabled: flag("Uva__LongPollEnabled", true)?,
            lease_seconds: number("Lease__RequestSeconds", 1200)? as u32,
        };

        config.refuse_what_cannot_work()?;
        Ok(config)
    }

    /// The three ways a configuration can be accepted and still be wrong.
    ///
    /// Checked at start-up rather than discovered in an hour: each of these
    /// fails somewhere far from its cause — a lease shorter than the timeout
    /// looks like the archive double-judging, and a poll floor below twenty
    /// looks like nothing at all until somebody else's server complains.
    fn refuse_what_cannot_work(&self) -> anyhow::Result<()> {
        if self.poll_min < POLL_FLOOR_SECONDS {
            bail!(
                "AJ_Uva__PollMinSeconds is {}, below the floor of {POLL_FLOOR_SECONDS}. \
                 onlinejudge.org publishes no rate limit, so this one is not lowered.",
                self.poll_min
            );
        }
        if self.poll_max < self.poll_min {
            bail!(
                "AJ_Uva__PollMaxSeconds is {}, below AJ_Uva__PollMinSeconds of {}",
                self.poll_max,
                self.poll_min
            );
        }
        if u64::from(self.lease_seconds) <= self.pending_timeout {
            bail!(
                "AJ_Lease__RequestSeconds is {}, which does not exceed \
                 AJ_Uva__PendingTimeoutSeconds of {}. The Server would reclaim the job \
                 while this Runner was still waiting on the archive, and the next \
                 Runner to claim it would submit the same solution again.",
                self.lease_seconds,
                self.pending_timeout
            );
        }
        // **Renewal rides on this cadence.** A held lease is renewed at the top
        // of the same cycle that asks the archive, so the slowest poll interval
        // is also the slowest renewal. An operator being polite to uHunt by
        // raising this — the obvious, well-meant change — stretches the renewal
        // interval with it, and a lease that expires between two renewals is
        // reclaimed, claimed by another Runner, and **the same solution goes to
        // onlinejudge.org a second time**. That is the failure this Runner's
        // lease handling exists to prevent, reachable through configuration
        // alone, with nothing in any log to say it happened.
        //
        // Four, matching the keeper in `AlgoJudge-Runner`: three renewals fit
        // inside every lease, so two may fail in a row with the deadline still
        // comfortably ahead.
        if self.poll_max.saturating_mul(4) > u64::from(self.lease_seconds) {
            bail!(
                "AJ_Uva__PollMaxSeconds is {}, which does not fit four times inside \
                 AJ_Lease__RequestSeconds of {}. A lease is renewed on the polling \
                 cycle, so this one could expire between two renewals — and the next \
                 Runner to claim the job would submit the same solution again. \
                 Lower the poll interval, or raise the lease (the Server clamps it \
                 at 3600, so this cannot exceed 900).",
                self.poll_max,
                self.lease_seconds
            );
        }
        if self.max_pending == 0 {
            bail!("AJ_Uva__MaxPending is 0, so no job could ever be claimed");
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
            problem_types: vec!["uva@1".into()],
            tags: vec![],
            key_path: "/tmp/identity.key".into(),
            uva_base_url: "https://onlinejudge.org/".into(),
            uhunt_base_url: "https://uhunt.onlinejudge.org/".into(),
            uva_username: "robot".into(),
            uva_password: "secret".into(),
            uva_user_id: None,
            poll_min: 20,
            poll_max: 60,
            poll_escalate_after: 120,
            submit_min_interval: 5,
            pending_timeout: 900,
            max_pending: 20,
            long_poll_enabled: true,
            lease_seconds: 1200,
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

    #[test]
    fn a_poll_floor_below_twenty_is_refused() {
        let mut config = base();
        config.poll_min = 5;
        let refused = config.refuse_what_cannot_work().unwrap_err().to_string();
        assert!(refused.contains("PollMinSeconds"), "{refused}");
        assert!(refused.contains("not lowered"), "{refused}");
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
    /// down to be kind to uHunt slows the renewing down with it. At three
    /// hundred seconds against a twenty-minute lease there is still room; at six
    /// hundred there is not, and nothing about the failure would point here.
    #[test]
    fn a_poll_interval_that_does_not_fit_inside_the_lease_is_refused() {
        let mut config = base();
        config.poll_max = 300;
        config
            .refuse_what_cannot_work()
            .expect("four times three hundred fits inside twenty minutes");

        config.poll_max = 600;
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
        let refused = number_in("3O", "Uva__PollMinSeconds")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("AJ_Uva__PollMinSeconds"), "{refused}");
        assert!(refused.contains("3O"), "{refused}");
    }

    #[test]
    fn a_base_url_is_given_its_trailing_slash() {
        assert_eq!(trailing_slash("https://x.test"), "https://x.test/");
        assert_eq!(trailing_slash("https://x.test/"), "https://x.test/");
    }
}
