//! The loop: take work, hand it to the judge, wait, report.
//!
//! **One loop with two clocks**, rather than two tasks with a lock between them.
//! Asking our own Server for work is cheap and may be frequent; asking somebody
//! else's judge is neither, and is floored at twenty seconds. Keeping both in
//! one place means the pending set needs no synchronisation and the order of
//! operations is on the screen rather than in a scheduler.
//!
//! **Nothing here names an archive.** Every reach outside the installation goes
//! through `crate::integration::Judge`, so adding a second judging system is a
//! module beside `crate::uva` rather than a change to this file.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aj_protocol::wire::{AttachToJob, ClaimedJob, Register, ReportResult};
use aj_protocol::{Backoff, Cache, Identity, Server};

use crate::config::Config;
use crate::integration::{Chosen, Judge, Outcome, Refused, Setup};
use crate::lease::{self, Action, Standing};
use crate::pending::{Entry, Matched, Pending};

/// Why a claimed job never reaches the judge, and **whose fault that is**.
///
/// The distinction used to live in two places and they disagreed: one reader
/// decided whether a refusal was a verdict, another decided what to submit, and
/// the second one's answer for the same condition was an infrastructure failure.
/// One reader cannot disagree with itself.
enum Blocked {
    /// The activity's own rules refuse it. **A verdict, not a failure**: the
    /// participant chose it, their code may be perfect, and what they broke is a
    /// rule of the activity — the same answer `standard-io@1` gives.
    Verdict(String),
    /// The platform cannot forward it. An infrastructure failure, and the
    /// submission stays rejudgeable.
    Failure(String),
}

/// What the Server records this Runner as being.
///
/// One string for every judge: what varies is which problem types it declares,
/// and the Server already stores those.
pub const PRODUCT: &str = "algojudge-external-runner";

/// What this Runner declares it serves.
///
/// The judge's own type unless an operator narrowed or widened it with
/// `AJ_Runner__ProblemTypes`.
fn declared<J: Judge>(config: &Config, judge: &J) -> Vec<String> {
    if config.problem_types.is_empty() {
        vec![judge.problem_type().to_owned()]
    } else {
        config.problem_types.clone()
    }
}

/// Registered and holding a token, however long that takes.
pub async fn admitted<J: Judge>(
    server: &Server,
    identity: &Identity,
    config: &Config,
    judge: &J,
) -> anyhow::Result<()> {
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(60));

    loop {
        let asked = server
            .register(&Register {
                name: config.runner_name.clone(),
                product: PRODUCT.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                public_key: identity.public_key(),
                problem_types: declared(config, judge),
                // **The whole point of this Runner, declared.** Every submission
                // it takes leaves the installation, and the Server pairs work with
                // workers on this: without it an external problem is never handed
                // over, and the queue simply looks empty.
                external: true,
                tags: config.tags.clone(),
                machine: None,
            })
            .await;

        match asked {
            Ok(registered) if registered.approved() => break,
            Ok(_) => {
                tracing::info!("registered and waiting to be approved by a manager");
                backoff.wait().await;
            }
            Err(e) if e.retryable() || e.unavailable() => {
                tracing::warn!(%e, "the Server could not take the registration");
                backoff.wait().await;
            }
            Err(e) => return Err(anyhow::anyhow!("the Server refused the registration: {e}")),
        }
    }

    backoff.reset();
    loop {
        match server.authenticate(identity).await {
            Ok(()) => return Ok(()),
            Err(e) if e.not_approved() => {
                tracing::info!("still waiting to be approved");
                backoff.wait().await;
            }
            Err(e) if e.retryable() || e.unavailable() => {
                tracing::warn!(%e, "the handshake could not be completed");
                backoff.wait().await;
            }
            Err(e) => return Err(anyhow::anyhow!("the Server refused the handshake: {e}")),
        }
    }
}

pub struct Runner<J: Judge> {
    pub server: Server,
    pub cache: Arc<Cache>,
    pub judge: J,
    pub config: Config,
    pending: Pending,
    /// Renewal attempts in a row that could not reach the Server.
    unreachable: u32,
}

impl<J: Judge> Runner<J> {
    pub fn new(server: Server, cache: Arc<Cache>, judge: J, config: Config) -> Self {
        Self {
            server,
            cache,
            judge,
            config,
            pending: Pending::default(),
            unreachable: 0,
        }
    }

    pub async fn work(&mut self, identity: &Identity) -> anyhow::Result<()> {
        let mut claiming = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut ask_judge_at = Instant::now();
        let mut beat_at = Instant::now();

        loop {
            if !self.pending.is_empty() && Instant::now() >= ask_judge_at {
                self.renew_everything().await;
                self.harvest().await;
                self.expire().await;
                ask_judge_at = Instant::now() + self.cycle();
            }

            if self.pending.len() < self.config.external.max_pending {
                match self.server.claim(Some(self.config.lease_seconds)).await {
                    Ok(Some(job)) => {
                        // **Asked and granted, side by side.** The Server may
                        // apply its own default when it reads no request, and
                        // every lease guard in `Config` computes with the number
                        // on this side — so a disagreement here is invisible
                        // from either log alone.
                        tracing::info!(
                            job = %job.job_id,
                            asked = self.config.lease_seconds,
                            granted = %job.lease_expires_at,
                            "claimed"
                        );
                        claiming.reset();
                        self.take(job).await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) if e.needs_handshake() => {
                        self.server.forget_token();
                        admitted(&self.server, identity, &self.config, &self.judge).await?;
                        continue;
                    }
                    Err(e) => tracing::warn!(%e, "could not ask for work"),
                }
            }

            if beat_at.elapsed() >= Duration::from_secs(60) {
                if let Err(e) = self.server.heartbeat().await {
                    tracing::warn!(%e, "the heartbeat did not land");
                }
                beat_at = Instant::now();
            }

            // Nothing outstanding: sleep as the ordinary Runner does. Something
            // outstanding: wake in time for the judge, and no later.
            if self.pending.is_empty() {
                claiming.wait().await;
            } else {
                let until = ask_judge_at.saturating_duration_since(Instant::now());
                tokio::time::sleep(until.min(Duration::from_secs(5))).await;
            }
        }
    }

    /// How long until the judge is asked again.
    ///
    /// **The long-poll trigger is not built yet**, so the interval is computed as
    /// if the accelerator were off — which is what makes escalation earn its
    /// keep. Wiring the trigger later changes this line and nothing else.
    fn cycle(&self) -> Duration {
        let oldest = self
            .pending
            .iter()
            .map(|(_, entry)| entry.sent.elapsed())
            .max()
            .unwrap_or_default();
        crate::schedule::interval(
            false,
            oldest,
            Duration::from_secs(self.config.external.poll_min),
            Duration::from_secs(self.config.external.poll_max),
            Duration::from_secs(self.config.external.poll_escalate_after),
        )
    }

    /// Everything that has to happen between claiming a job and waiting for it.
    async fn take(&mut self, job: ClaimedJob) {
        let token = job.lease_token.clone();

        // **A language the assignment excluded is a verdict, not a failure**,
        // and it is decided before anything is forwarded — nothing should reach
        // the judge that the activity's own rules already refuse.
        //
        // The same answer `standard-io@1` gives for the same mistake: the
        // participant chose it, their code may be perfect, and what they broke
        // is a rule of the activity. Two problem types answering this
        // differently would make the verdict a property of who judged rather
        // than of what happened.
        let (setup, language) = match self.prepare(&job) {
            Ok(prepared) => prepared,
            Err(Blocked::Verdict(refusal)) => {
                tracing::info!(job = %job.job_id, %refusal, "refused by the activity's rules");
                self.send(
                    &job.job_id,
                    &ReportResult::judged(&token, 0.0, 1.0, crate::integration::POLICY_VIOLATION),
                )
                .await;
                return;
            }
            Err(Blocked::Failure(why)) => {
                tracing::warn!(job = %job.job_id, %why, "not forwarded");
                self.fail(&job.job_id, &token, &why).await;
                return;
            }
        };

        match self.forward(&job, setup, language).await {
            Ok(entry) => {
                if let Err(e) = self.server.progress(&job.job_id, &token).await {
                    tracing::warn!(%e, "could not say the work had started");
                }
                tracing::info!(
                    job = %job.job_id,
                    judge = self.judge.name(),
                    sid = entry.0,
                    "handed over",
                );
                self.pending.insert(entry.0, entry.1);
            }
            Err(why) => {
                tracing::warn!(job = %job.job_id, %why, "not forwarded");
                self.fail(&job.job_id, &token, &why.to_string()).await;
            }
        }
    }

    /// Everything a job has to say before anything leaves the installation.
    ///
    /// **One reader, because two disagreed about the same rule.** `allowed`
    /// read the two documents and answered the language question to decide
    /// whether a refusal was a verdict; `forward` read the same two documents
    /// and asked the same question again to decide what to submit — four
    /// `serde_json::from_value` over cloned documents per job. `forward`'s own
    /// `NotAllowed` arm carried the comment *"reaching here would mean the two
    /// disagreed about the same rule"*, and it was unreachable by construction
    /// while turning that state into an infrastructure failure — the opposite
    /// of the answer the identical condition earned twenty lines earlier.
    ///
    /// Two constraints the collapse had to preserve, and does. An unreadable
    /// configuration is reported **once**, with the message that names the
    /// missing field — `allowed` swallowed that error precisely because
    /// `forward` owned the message. And `NotSubmittable` stays a `Failure`: the
    /// participant chose from a list the platform gave them, so it is the
    /// platform's fault and the submission stays rejudgeable.
    ///
    /// `props.language`, since 2026-08-22. The Server carries the document
    /// without reading a member of it, so which member names the language is
    /// the problem type's to know — and an external type calls it the same
    /// thing `standard-io@1` does.
    fn prepare(&self, job: &ClaimedJob) -> Result<(Setup, i64), Blocked> {
        // Two documents, two questions: the version says which problem, the
        // assignment says how this course counts it.
        let setup = self
            .judge
            .read(job.problem_version_props.as_ref(), job.config.as_ref())
            .map_err(|e| Blocked::Failure(e.to_string()))?;

        let language = match self.judge.language(&setup, language_of(job.props.as_ref())) {
            Chosen::Accepted(number) => number,
            Chosen::NotAllowed { wanted, allowed } => {
                return Err(Blocked::Verdict(format!(
                    "this problem does not accept {wanted} here; it accepts {allowed:?}"
                )))
            }
            Chosen::NotSubmittable(why) => return Err(Blocked::Failure(why)),
        };
        Ok((setup, language))
    }

    async fn forward(
        &mut self,
        job: &ClaimedJob,
        setup: Setup,
        language: i64,
    ) -> anyhow::Result<(i64, Entry)> {
        let pid = self.judge.problem(setup.number).await?;

        let submitted = job
            .files
            .iter()
            .find(|f| f.name == "source")
            .or_else(|| job.files.first())
            .ok_or_else(|| anyhow::anyhow!("the submission carries no file"))?;
        let held = self
            .cache
            .fetch(&self.server, &submitted.file_id, &submitted.sha256)
            .await?;
        let source = std::fs::read_to_string(held.path())?;

        let name = self.judge.name();
        let sid = self
            .judge
            .submit(
                setup.number,
                language,
                &source,
                Duration::from_secs(self.config.external.submit_min_interval),
            )
            .await
            // **The judge is named here rather than by the error type.** One
            // wording for every integration would drop the only word an
            // operator reading a failed submission needs.
            .map_err(|refused| match refused {
                Refused::SessionLapsed => anyhow::anyhow!("the {name} session had lapsed"),
                Refused::AcceptedWithoutAnId => anyhow::anyhow!(
                    "{name} received the submission and did not say which id it gave it. \
                     It is on the account and cannot be matched to this job — look at the \
                     account before rejudging, or the participant gets two rows for one \
                     attempt"
                ),
                Refused::Site(why) => anyhow::anyhow!("{name} refused the submission: {why}"),
            })?;

        Ok((
            sid,
            Entry {
                job_id: job.job_id.clone(),
                lease_token: job.lease_token.clone(),
                problem_number: setup.number,
                pid,
                language_id: language,
                sent: Instant::now(),
                announced: true,
                accepted: setup.accepted,
                trail: vec![
                    format!("submitted problem {} as language {language}", setup.number),
                    format!("sid: {sid}"),
                ],
            },
        ))
    }

    /// One request, however many submissions are outstanding.
    async fn harvest(&mut self) {
        let outstanding: Vec<i64> = self.pending.sids().collect();
        if outstanding.is_empty() {
            return;
        }
        let answers = match self.judge.answers(&outstanding).await {
            Ok(answers) => answers,
            // Not reaching the judge says nothing about anybody's solution.
            Err(e) => {
                tracing::warn!(%e, judge = self.judge.name(), "could not read the judge");
                return;
            }
        };

        let mut done: Vec<(i64, Option<serde_json::Value>, ReportResult)> = Vec::new();
        for answer in &answers {
            let id = self.judge.id_of(answer);
            let entry = match self.pending.matched(id, self.judge.problem_of(answer)) {
                Matched::Stranger => continue,
                Matched::Disagrees { expected, found } => {
                    tracing::error!(
                        sid = id,
                        expected,
                        found,
                        "an answer names a different problem than the one we sent; \
                        not treating it as an answer"
                    );
                    continue;
                }
                Matched::Ours(entry) => entry.clone(),
            };

            if let Some(held) = self.pending.get_mut(id) {
                held.trail.push(self.judge.evidence(answer));
            }

            match self.judge.outcome(answer) {
                Outcome::Pending => {}
                Outcome::Judged {
                    verdict,
                    abbreviation,
                } => {
                    let solved = crate::integration::solved(abbreviation, &entry.accepted);
                    let document =
                        self.judge
                            .details(&entry, answer, verdict, abbreviation, solved);
                    done.push((
                        id,
                        Some(document),
                        ReportResult::judged(
                            &entry.lease_token,
                            if solved { 1.0 } else { 0.0 },
                            1.0,
                            verdict,
                        ),
                    ));
                }
                Outcome::Failed { reason, permanent } => {
                    if permanent {
                        tracing::error!(
                            problem = entry.problem_number,
                            "{reason}; this will not be retried"
                        );
                    }
                    let document = self.judge.details_of_failure(&entry, id, reason);
                    done.push((
                        id,
                        Some(document),
                        ReportResult::failed(&entry.lease_token, reason),
                    ));
                }
            }
        }

        for (sid, document, report) in done {
            let Some(entry) = self.pending.take(sid) else {
                continue;
            };
            self.attach(&entry, document).await;
            self.send(&entry.job_id, &report).await;
        }
    }

    /// The judge did not answer in time.
    async fn expire(&mut self) {
        let timeout = Duration::from_secs(self.config.external.pending_timeout);
        for sid in self.pending.timed_out(timeout, Instant::now()) {
            let Some(entry) = self.pending.take(sid) else {
                continue;
            };
            let why = format!(
                "{} did not judge submission {sid} within {} seconds",
                self.judge.name(),
                self.config.external.pending_timeout
            );
            let document = self.judge.details_of_failure(&entry, sid, &why);
            self.attach(&entry, Some(document)).await;
            self.fail(&entry.job_id, &entry.lease_token, &why).await;
        }
    }

    /// Renewed unconditionally, because renewal never shortens a lease.
    async fn renew_everything(&mut self) {
        let held: Vec<(i64, String, String)> = self
            .pending
            .iter()
            .map(|(sid, entry)| (sid, entry.job_id.clone(), entry.lease_token.clone()))
            .collect();
        let ceiling = lease::ceiling(self.config.lease_seconds, self.config.external.poll_max);

        for (sid, job_id, token) in held {
            // Success is an answer like any other, so it goes through the same
            // decision rather than short-circuiting past it.
            let standing = match self
                .server
                .renew(&job_id, &token, Some(self.config.lease_seconds))
                .await
            {
                Ok(_) => Standing::Held,
                Err(e) => {
                    tracing::warn!(%e, job = %job_id, "the lease was not renewed");
                    Standing::of(&e)
                }
            };
            self.unreachable = match standing {
                Standing::Unreachable => self.unreachable.saturating_add(1),
                _ => 0,
            };

            match lease::act(standing, self.unreachable, ceiling) {
                Action::KeepWaiting => {}
                Action::DropSilently => {
                    tracing::warn!(job = %job_id, "the lease is gone; another Runner has this job");
                    self.pending.take(sid);
                }
                Action::GiveUp => {
                    tracing::error!(job = %job_id, "the Server has been unreachable too long");
                    self.pending.take(sid);
                }
            }
        }
    }

    /// The two artefacts, **before** the report.
    ///
    /// The order is not a preference: the Server accepts an attachment only
    /// while the job is `Running`, and reporting ends that. Get it the wrong way
    /// round and the log explaining a failure is the thing that goes missing.
    ///
    /// A lost attachment is a warning, never a failure: an answer without its
    /// evidence is worth more than no answer at all.
    async fn attach(&self, entry: &Entry, document: Option<serde_json::Value>) {
        let mut carried: Vec<(&str, &str, Vec<u8>)> = Vec::new();
        if !entry.trail.is_empty() {
            carried.push(("log", "text/plain", entry.trail.join("\n").into_bytes()));
        }
        if let Some(document) = &document {
            match serde_json::to_vec_pretty(document) {
                Ok(bytes) => carried.push(("details", "application/json", bytes)),
                Err(e) => tracing::warn!(%e, "the result document could not be written"),
            }
        }

        for (name, mime, bytes) in carried {
            let file_name = if name == "details" {
                "details.json"
            } else {
                "log.txt"
            };
            let uploaded = match self.server.upload(file_name, mime, bytes).await {
                Ok(uploaded) => uploaded,
                Err(e) => {
                    tracing::warn!(name, %e, "an attachment could not be uploaded");
                    continue;
                }
            };
            if let Err(e) = self
                .server
                .attach_to_job(
                    &entry.job_id,
                    &AttachToJob {
                        lease_token: entry.lease_token.clone(),
                        file_id: uploaded.id,
                        name: name.to_owned(),
                    },
                )
                .await
            {
                tracing::warn!(name, %e, "an attachment could not be named on the attempt");
            }
        }
    }

    async fn fail(&self, job_id: &str, token: &str, why: &str) {
        self.send(job_id, &ReportResult::failed(token, why)).await;
    }

    /// Retried for as long as it takes: the report is idempotent, so trying
    /// again is always safe, and a lost lease is the one answer to stop on.
    async fn send(&self, job_id: &str, report: &ReportResult) {
        let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(30));
        for _ in 0..10 {
            match self.server.report(job_id, report).await {
                Ok(_) => return,
                Err(e) if e.lease_lost() => {
                    tracing::warn!(job = %job_id, "the lease was gone; the answer is dropped");
                    return;
                }
                Err(e) => {
                    tracing::warn!(%e, job = %job_id, "the report did not land");
                    backoff.wait().await;
                }
            }
        }
    }
}

/// Which member of a submission's `props` names the language.
///
/// The same member `standard-io@1` reads, deliberately: one label map in the
/// Client serves both types, and a participant reading their own submission
/// should not have to know which Runner judged it.
fn language_of(props: Option<&serde_json::Value>) -> Option<&str> {
    props?.get("language")?.as_str()
}
