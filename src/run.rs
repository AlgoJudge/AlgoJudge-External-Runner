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

use aj_protocol::stopping::Stopping;
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
///
/// **Public because `main` logs it and `admitted` sends it.** `main` restated
/// this body token for token rather than calling it — a separate crate cannot
/// reach a private function — so the two could drift, and the drift's shape is
/// the invisible one: a start-up line telling an operator the Runner declares
/// one thing while the registration says another, and a queue that never drains.
pub fn declared<J: Judge>(config: &Config, judge: &J) -> Vec<String> {
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
    /// **Shared, so the heartbeat can hold one too.** Liveness runs on a timer
    /// of its own rather than at the bottom of this loop — see `heartbeat`.
    pub server: Arc<Server>,
    pub cache: Arc<Cache>,
    pub judge: J,
    pub config: Config,
    pending: Pending,
}

impl<J: Judge> Runner<J> {
    pub fn new(server: Arc<Server>, cache: Arc<Cache>, judge: J, config: Config) -> Self {
        Self {
            server,
            cache,
            judge,
            config,
            pending: Pending::default(),
        }
    }

    /// Runs until it is told to stop, and hands back what it was holding.
    ///
    /// **Every job, not one.** The sandboxing Runner holds a single submission
    /// and gives that one back; this Runner holds a pool of up to
    /// `AJ_External__MaxPending`, and a lease left to expire costs each of those
    /// participants the whole of it.
    pub async fn work(&mut self, identity: &Identity, stopping: &Stopping) -> anyhow::Result<()> {
        let mut claiming = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut ask_judge_at = Instant::now();

        // **Liveness on a timer of its own.** It used to be a threshold checked
        // at the bottom of this loop, which was fine while an iteration was a
        // second or two. A held claim made the iteration tens of seconds, so a
        // sixty-second threshold fired every second pass — an effective beat of
        // about a hundred and ten seconds against a Server that calls a Runner
        // disconnected after a hundred and twenty.
        //
        // It also covers what the loop never could: this Runner spends most of
        // its life waiting on somebody else's archive, and a cycle of that is
        // not a moment it should have to remember to say it is alive in.
        let beating = heartbeat(Arc::clone(&self.server), stopping.clone());

        loop {
            // **Checked first, so a stop is acted on before another cycle of
            // somebody else's archive.** Asking the judge is the slowest thing
            // this loop does, and the jobs it would ask about are the ones being
            // given back.
            if stopping.now() {
                beating.abort();
                self.give_everything_back().await;
                return Ok(());
            }

            if !self.pending.is_empty() && Instant::now() >= ask_judge_at {
                self.renew_everything().await;
                self.harvest().await;
                self.expire().await;
                ask_judge_at = Instant::now() + self.cycle();
            }

            // **And not while stopping**, which the top of the loop cannot
            // decide on its own: asking the judge about the pending set happens
            // in between, and it is the slowest call this Runner makes.
            // Whether the Server held the last claim open, which decides
            // whether there is anything left to wait for below.
            let mut held = false;

            if !stopping.now() && self.pending.len() < self.config.external.max_pending {
                let wait =
                    (self.config.poll_wait > 0).then(|| Duration::from_secs(self.config.poll_wait));
                let asked = Instant::now();
                // **Raced against the stop, which it was not until 2026-09-04.**
                // The check above happens before a call the Server may hold open
                // for the whole of `poll_wait`, so a stop arriving during the
                // hold cancelled nothing: the process sat there uninterruptible
                // while its grace ran out, and everything it had already
                // forwarded to the judge stayed leased for the full lease
                // instead of being handed back.
                //
                // Settled rather than dropped when the stop wins, for the reason
                // the sandboxing Runner gives at its own claim: the Server
                // commits a handout before writing the answer to it, so a
                // dropped request is sometimes a job this Runner owns and cannot
                // release, never having learned the lease token. Two seconds is
                // a response in flight; it is not a poll.
                // Cloned so the future in flight borrows this handle rather
                // than `self` — the stopping arm below hands work back, and
                // that needs the Runner itself.
                let server = Arc::clone(&self.server);
                let mut claim = std::pin::pin!(server.claim(Some(self.config.lease_seconds), wait));
                let claimed = tokio::select! {
                    // An answer already in hand beats a stop that arrived with
                    // it, rather than a coin flip that throws the job away.
                    biased;
                    claimed = &mut claim => claimed,
                    _ = stopping.wait() => {
                        if let Ok(Ok(Some(job))) =
                            tokio::time::timeout(SETTLE, &mut claim).await
                        {
                            // **Released, never forwarded.** Taking it would put
                            // a real submission on the judge's account that this
                            // installation is about to abandon, and then hand
                            // the job back for the next Runner to forward again
                            // — two submissions on somebody else's site for one
                            // attempt.
                            gave_back(&server, &job).await;
                        }
                        beating.abort();
                        self.give_everything_back().await;
                        return Ok(());
                    }
                };
                match claimed {
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

                        // **Asked again between the answer and the forward.**
                        // `biased` hands over a job that arrived at the same
                        // instant as the stop, which is right — but forwarding
                        // it would submit to the judge on the way out and then
                        // release the job below, so the next Runner forwards it
                        // a second time. Giving it straight back costs one round
                        // trip and nothing else.
                        if stopping.now() {
                            gave_back(&server, &job).await;
                            beating.abort();
                            self.give_everything_back().await;
                            return Ok(());
                        }

                        self.take(job).await;
                        continue;
                    }
                    Ok(None) => {
                        // **Told apart by how long it took**, not by the
                        // setting: a Server that does not know about
                        // `waitSeconds` answers at once, and so does one that is
                        // draining, and neither should be asked again
                        // immediately.
                        held = wait.is_some_and(|wait| asked.elapsed() >= wait / 2);
                    }
                    Err(e) if e.needs_handshake() => {
                        self.server.forget_token();
                        if let Err(e) =
                            admitted(&self.server, identity, &self.config, &self.judge).await
                        {
                            beating.abort();
                            return Err(e);
                        }
                        continue;
                    }
                    Err(e) => tracing::warn!(%e, "could not ask for work"),
                }
            }

            // Nothing outstanding: sleep as the ordinary Runner does. Something
            // outstanding: wake in time for the judge, and no later.
            //
            // **Both waits are cut short by the word.** The backoff reaches
            // thirty seconds and the judge's interval reaches five, and a Runner
            // that sat out either of them before releasing would be holding
            // leases for no reason at all.
            if self.pending.is_empty() {
                // **No backoff after a claim the Server held.** The wait *was*
                // the interval; sleeping again would leave this Runner deaf for
                // the thirty seconds the backoff has climbed to, and a
                // submission arriving in that window waits it out — the old
                // latency, on the new machinery.
                if !held {
                    tokio::select! {
                        _ = claiming.wait() => {}
                        _ = stopping.wait() => {}
                    }
                }
            } else {
                let until = ask_judge_at.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = tokio::time::sleep(until.min(Duration::from_secs(5))) => {}
                    _ = stopping.wait() => {}
                }
            }
        }
    }

    /// Hands back everything this Runner is waiting on, because it is stopping.
    ///
    /// **A systemic act, not a processing error.** Nothing went wrong with any
    /// of these submissions; the platform is taking their Runner away. So the
    /// job returns to the queue with its delivery uncounted, rather than being
    /// reported as an infrastructure failure that spends one of its attempts.
    ///
    /// **What it costs is one duplicate submission to somebody else's site**,
    /// and that cost is not new: the pending set lives in memory, so a stop of
    /// any kind already leaves the answer that is still coming with nowhere to
    /// land, and the job is already re-forwarded by whoever claims it next.
    /// What changes is when — now, rather than when the lease expires.
    async fn give_everything_back(&mut self) {
        let held: Vec<(i64, String, String)> = self
            .pending
            .iter()
            .map(|(sid, entry)| (sid, entry.job_id.clone(), entry.lease_token.clone()))
            .collect();

        for (sid, job_id, token) in held {
            // A refusal here is not a failure to report. The one that matters
            // says the lease is gone, which means the Server has already put the
            // job back — the outcome this was asking for.
            match self.server.release(&job_id, &token).await {
                Ok(()) => tracing::info!(job = %job_id, sid, "gave the job back"),
                Err(e) if e.lease_lost() => {
                    tracing::info!(job = %job_id, sid, "the job was already back")
                }
                Err(e) => tracing::warn!(
                    job = %job_id, sid, %e,
                    "could not give the job back; it returns when the lease expires",
                ),
            }
            self.pending.take(sid);
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
                sent: Instant::now(),
                unreachable: 0,
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
                    // The distinction goes to the Server, not only to this
                    // Runner's stderr — see `integration::failure_reason`.
                    let why = crate::integration::failure_reason(reason, permanent);
                    if permanent {
                        tracing::error!(problem = entry.problem_number, "{why}");
                    } else {
                        tracing::warn!(problem = entry.problem_number, "{why}");
                    }
                    let document = self.judge.details_of_failure(&entry, id, &why);
                    done.push((
                        id,
                        Some(document),
                        ReportResult::failed(&entry.lease_token, &why),
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
        let mut giving_up: Vec<(i64, u32)> = Vec::new();

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
            let consecutive = self
                .pending
                .renewal(sid, !matches!(standing, Standing::Unreachable));

            match lease::act(standing, consecutive, ceiling) {
                Action::KeepWaiting => {}
                Action::DropSilently => {
                    tracing::warn!(job = %job_id, "the lease is gone; another Runner has this job");
                    self.pending.take(sid);
                }
                Action::GiveUp => giving_up.push((sid, consecutive)),
            }
        }

        // **Collected, and acted on after the loop.** Giving a job up means an
        // upload, an attach and a report against a Server that has just been
        // unreachable for the whole ceiling — `send` retries ten times with a
        // backoff reaching thirty seconds, so one of these can hold this loop
        // for three and a half minutes. It is worth paying, because the ceiling
        // is reached while the lease is still valid and the report has a real
        // chance of landing. It is not worth paying **before** the jobs that are
        // still fine have been renewed.
        for (sid, consecutive) in giving_up {
            let Some(entry) = self.pending.take(sid) else {
                continue;
            };
            tracing::error!(
                job = %entry.job_id,
                consecutive,
                "the Server has been unreachable too long; giving the job up",
            );
            let why = format!(
                "the Server could not be reached for {consecutive} renewal cycles in a row, so \
                 this Runner stopped holding submission {sid} on {}. The submission is on the \
                 account and was never collected; a rejudge would send it again",
                self.judge.name()
            );
            let document = self.judge.details_of_failure(&entry, sid, &why);
            self.attach(&entry, Some(document)).await;
            self.fail(&entry.job_id, &entry.lease_token, &why).await;
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

/// How often this Runner says it is alive.
///
/// **Not configurable, unlike the sandboxing Runner's.** That one is set per
/// fleet member because a fleet is many processes on one host; this is one
/// process against one judging system, and a number nobody would ever want to
/// change is better as a constant than as a key in `.env.example`.
/// Hands one job straight back, without ever having forwarded it.
///
/// **The same refusals `give_everything_back` tolerates**, for the same reason:
/// a lease that is already gone means the Server has put the job back, which is
/// the outcome this was asking for.
async fn gave_back(server: &Arc<Server>, job: &ClaimedJob) {
    match server.release(&job.job_id, &job.lease_token).await {
        Ok(()) => tracing::info!(job = %job.job_id, "gave the job back"),
        Err(e) if e.lease_lost() => tracing::info!(job = %job.job_id, "the job was already back"),
        Err(e) => tracing::warn!(
            job = %job.job_id, %e,
            "could not give the job back; it returns when the lease expires",
        ),
    }
}

/// How long a stop waits for a claim already in flight to answer.
///
/// **Long enough for a response, far too short for a poll.** The same number and
/// the same argument as the sandboxing Runner's; the reasoning is written at the
/// stopping arm in `work`, which is the only place it is used.
const SETTLE: Duration = Duration::from_secs(2);

const HEARTBEAT: Duration = Duration::from_secs(60);

/// Says this Runner is alive, on its own timer, until it is told to stop.
///
/// **Not tied to the loop, and not to the judge's cycle.** The Server calls a
/// Runner disconnected when what it last heard is two minutes old, and neither
/// a held claim nor a poll of somebody else's archive is a number chosen
/// against that.
fn heartbeat(server: Arc<Server>, stopping: Stopping) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(HEARTBEAT) => {}
                _ = stopping.wait() => return,
            }

            if let Err(e) = server.heartbeat().await {
                tracing::warn!(%e, "the heartbeat did not land");
            }
        }
    })
}

/// Which member of a submission's `props` names the language.
///
/// The same member `standard-io@1` reads, deliberately: one label map in the
/// Client serves both types, and a participant reading their own submission
/// should not have to know which Runner judged it.
fn language_of(props: Option<&serde_json::Value>) -> Option<&str> {
    props?.get("language")?.as_str()
}
