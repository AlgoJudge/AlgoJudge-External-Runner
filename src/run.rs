//! The loop: take work, hand it to the judge, wait, report.
//!
//! **Three concurrent waits**, joined in `work`: one asks our own Server for
//! work, one asks the judge about what is outstanding, one holds the judge's
//! live channel. They are separate because asking the Server may be held open
//! for `AJ_Poll__WaitSeconds`, and a task that waited there before listening was
//! deaf for that long with a submission already at the archive.
//!
//! The pending set is therefore shared, behind a lock that is never held across
//! an await.
//!
//! **Nothing here names an archive.** Every reach outside the installation goes
//! through `crate::integration::Judge`, so adding a second judging system is a
//! module beside `crate::uva` rather than a change to this file.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

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

/// The longer of this Runner's own backoff and what the Server asked for.
///
/// Advice, not an instruction, and the arithmetic says which: an operator's
/// `Retry-After: 300` is honoured because it is longer, and a proxy's
/// `Retry-After: 0` cannot turn a retry into a spin because it is not.
fn how_long(e: &aj_protocol::Error, backoff: &mut Backoff) -> Duration {
    let mine = backoff.next_delay();
    e.retry_after()
        .filter(|asked| *asked > mine)
        .unwrap_or(mine)
}

/// Waits out a Server that is up and declining to serve, and says which it is.
///
/// **Answers `false` when the word came instead**, which is not the same as the
/// window having ended: the caller is going away, and a window advertising
/// `Retry-After: 300` must not hold a stopping Runner past its grace.
///
/// Asks `/health` rather than guessing. It is anonymous, it answers at every
/// level, and it carries the operator's own words — so a Runner that waits in
/// silence is indistinguishable from one that has died, and this one is not.
async fn wait_out(
    server: &Server,
    e: &aj_protocol::Error,
    backoff: &mut Backoff,
    stopping: &Stopping,
) -> bool {
    let delay = how_long(e, backoff);

    match server.health().await {
        Ok(health) if health.open() => {
            tracing::info!("the Server is serving again");
            backoff.reset();
            return true;
        }
        Ok(health) => tracing::info!(
            level = health.level(),
            reason = health.reason().unwrap_or("none given"),
            ?delay,
            "the Server is under maintenance; waiting",
        ),
        Err(unreachable) => tracing::warn!(
            %unreachable,
            ?delay,
            "the Server is not serving, and health could not be read either; waiting",
        ),
    }

    stopping.sleep(delay).await
}

/// Registered and holding a token, however long that takes.
pub async fn admitted<J: Judge>(
    server: &Server,
    identity: &Identity,
    config: &Config,
    judge: &J,
    stopping: &Stopping,
) -> anyhow::Result<()> {
    let mut backoff = Backoff::new(
        Duration::from_secs(config.claim_poll_min),
        Duration::from_secs(config.claim_poll_max),
    );

    loop {
        let asked = server
            .register(
                &Register {
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
                },
                identity,
            )
            .await;

        match asked {
            Ok(registered) if registered.approved() => {
                // **The Server's answer names the key it registered.** A
                // mismatch means this process is holding one key and the Server
                // another — a mounted volume from a different deployment is the
                // way it happens — and every claim after it would be refused
                // for a reason that reads like somebody else's fault.
                let mine = identity.fingerprint();
                if registered.fingerprint != mine {
                    return Err(anyhow::anyhow!(
                        "the Server registered the fingerprint {} and this Runner holds {mine}; the key on disk is not the key the Server knows",
                        registered.fingerprint,
                    ));
                }
                break;
            }
            // **Every wait here is raced against the word.** Waiting for a
            // manager to press approve is the longest thing this Runner ever
            // does, and until 2026-09-04 it took no stop handle at all — so a
            // Runner waiting to be let in could only be stopped by killing it,
            // and a kill leaves nothing behind but a container the runtime shot.
            Ok(_) => {
                tracing::info!("registered and waiting to be approved by a manager");
                tokio::select! {
                    _ = backoff.wait() => {}
                    _ = stopping.wait() => return Ok(()),
                }
            }
            // **The key is finished, and no amount of waiting revives it.**
            // There is no rotation: a new key is a new configuration and a new
            // registration, which is a person's decision.
            Err(e) if e.revoked() => {
                return Err(anyhow::anyhow!(
                    "this Runner's key has been revoked; it needs a new key and a new registration, which no retry can do: {e}"
                ));
            }
            Err(e) if e.retryable() || e.unavailable() || e.in_maintenance() => {
                if !wait_out(server, &e, &mut backoff, stopping).await {
                    return Ok(());
                }
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
                tokio::select! {
                    _ = backoff.wait() => {}
                    _ = stopping.wait() => return Ok(()),
                }
            }
            Err(e) if e.revoked() => {
                return Err(anyhow::anyhow!(
                    "this Runner's key has been revoked; it needs a new key and a new registration, which no retry can do: {e}"
                ));
            }
            Err(e) if e.retryable() || e.unavailable() || e.in_maintenance() => {
                tracing::warn!(%e, "the handshake could not be completed");
                tokio::select! {
                    _ = backoff.wait() => {}
                    _ = stopping.wait() => return Ok(()),
                }
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
    /// Shared by the three waits, and never locked across an await.
    /// `std::sync::Mutex` and not the async one, so holding it over an await is
    /// a compile error rather than a stall nobody notices.
    pending: Mutex<Pending>,
    /// Raised when a submission joins the set, so the two tasks that have work
    /// only while something is outstanding sleep instead of asking.
    arrived: Notify,
    /// Raised when the judge's live channel mentions our account. It means
    /// *ask now* and never *here is the verdict*: the channel drops events, so
    /// a verdict read from it is a verdict that can be missed.
    sign: Notify,
    /// Raised when the set empties, so the live channel is dropped at once
    /// rather than held for the rest of `AJ_External__PollMaxSeconds` with
    /// nothing left to hear about.
    settled: Notify,

    /// **Held rather than passed**, because the waits that have to hear it are
    /// not all in `work`: the report retry is four calls down, and threading a
    /// handle through `harvest`, `expire` and `give_up` to reach it would put
    /// the argument everywhere except where it is read.
    ///
    /// Set when `work` starts. Before that there is nothing to stop.
    stopping: OnceLock<Stopping>,
}

impl<J: Judge> Runner<J> {
    pub fn new(server: Arc<Server>, cache: Arc<Cache>, judge: J, config: Config) -> Self {
        Self {
            server,
            cache,
            judge,
            config,
            pending: Mutex::new(Pending::default()),
            arrived: Notify::new(),
            sign: Notify::new(),
            settled: Notify::new(),
            // Set by `work`, which is the only thing that can be stopped.
            stopping: OnceLock::new(),
        }
    }

    /// Runs until it is told to stop, and hands back what it was holding.
    ///
    /// Three waits, concurrent rather than in turn: asking the Server for work
    /// may be held open for `AJ_Poll__WaitSeconds`, and a task that waited there
    /// before listening to the judge was deaf for that long with a submission
    /// already at the archive.
    ///
    /// `try_join!` rather than `spawn`: all three wait on sockets, so nothing
    /// needs to be `'static` or cloned into a task.
    pub async fn work(&self, identity: &Identity, stopping: &Stopping) -> anyhow::Result<()> {
        // Kept, so the report retry four calls down hears it too.
        let _ = self.stopping.set(stopping.clone());

        // On a timer of its own: none of the three waits below is bounded
        // tightly enough to serve as one, and the Server calls a Runner
        // disconnected after two minutes.
        let beating = heartbeat(Arc::clone(&self.server), stopping.clone());

        let outcome = tokio::try_join!(
            self.intake(identity, stopping),
            self.collecting(stopping),
            self.listening(stopping),
        );

        beating.abort();
        self.give_everything_back().await;
        outcome.map(|_| ())
    }

    /// Asks the Server for work and forwards it to the judge.
    ///
    /// **The only task that submits**, so the archive's one-at-a-time rule and
    /// the interval between submissions are this task's alone to keep — and
    /// `Site::submit` keeps them under its own lock either way.
    async fn intake(&self, identity: &Identity, stopping: &Stopping) -> anyhow::Result<()> {
        let mut claiming = Backoff::new(
            Duration::from_secs(self.config.claim_poll_min),
            Duration::from_secs(self.config.claim_poll_max),
        );

        loop {
            if stopping.now() {
                return Ok(());
            }

            // Full: wait rather than spin. What protects the archive is the
            // gap between submissions, not this ceiling.
            if self.outstanding() >= self.config.external.max_pending {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = stopping.wait() => return Ok(()),
                }
                continue;
            }

            // Whether the Server held the claim open, which decides whether to
            // back off before asking again.
            let mut held = false;

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
                    // Not here: another task may be holding an answer for
                    // one of these. `work` releases once, after the join.
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
                    admitted(&self.server, identity, &self.config, &self.judge, stopping).await?;
                    continue;
                }
                // **Up and declining to serve is not the same as broken.**
                // The window is named in the log with the operator's own
                // reason, and the wait is the longer of ours and theirs.
                Err(e) if e.unavailable() || e.in_maintenance() => {
                    if !wait_out(&self.server, &e, &mut claiming, stopping).await {
                        return Ok(());
                    }
                }
                // The key is finished; no wait revives it, and carrying on
                // would be a loop asking to be refused.
                Err(e) if e.revoked() => {
                    return Err(anyhow::anyhow!(
                    "this Runner's key has been revoked; it needs a new key and a new registration, which no retry can do: {e}"
                ));
                }
                Err(e) => tracing::warn!(%e, "could not ask for work"),
            }

            // **No backoff after a claim the Server held.** The wait *was* the
            // interval; sleeping again would leave this task deaf for the thirty
            // seconds the backoff has climbed to, and work arriving in that window
            // would sit through it. Told apart by how long the answer took, in the
            // `Ok(None)` arm above, because a Server that does not know about
            // `waitSeconds` and one that is draining both answer at once.
            if !held {
                tokio::select! {
                    _ = claiming.wait() => {}
                    _ = stopping.wait() => return Ok(()),
                }
            }
        }
    }

    /// Asks the judge about everything outstanding, and reports what came back.
    ///
    /// Two deadlines. A sign from the channel pulls collecting forward;
    /// renewal keeps the interval's cadence, because `lease::ceiling` counts
    /// renewal cycles against `AJ_External__PollMaxSeconds` and renewing on
    /// every event would spend that budget in seconds.
    ///
    /// Both start due, so the first pass after a submission acts at once.
    async fn collecting(&self, stopping: &Stopping) -> anyhow::Result<()> {
        let mut collect_at = Instant::now();
        let mut renew_at = Instant::now();

        loop {
            if stopping.now() {
                return Ok(());
            }

            // Registered before the count is read: `notify_waiters` stores
            // nothing for a task that is not yet waiting.
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();

            if self.outstanding() == 0 {
                tokio::select! {
                    _ = arrived => {}
                    _ = stopping.wait() => return Ok(()),
                }
                continue;
            }

            // Registered before the work below, not at the select: a sign
            // raised while this task is renewing or harvesting would be lost.
            let sign = self.sign.notified();
            tokio::pin!(sign);
            sign.as_mut().enable();

            let now = Instant::now();
            if now >= renew_at {
                self.renew_everything().await;
                renew_at = Instant::now() + self.cycle();
            }
            if now >= collect_at {
                self.harvest().await;
                self.expire().await;
                collect_at = Instant::now() + self.cycle();
            }

            // After both: renewal drops entries too, so the set can empty
            // without the collect branch running.
            if self.outstanding() == 0 {
                self.settled.notify_waiters();
            }

            if stopping.now() {
                return Ok(());
            }

            let until = collect_at
                .min(renew_at)
                .saturating_duration_since(Instant::now());
            tokio::select! {
                _ = tokio::time::sleep(until) => {}
                // Now, not at the top of the next interval: waking early and
                // then waiting anyway would spend the channel on nothing.
                _ = sign => collect_at = Instant::now(),
                _ = stopping.wait() => return Ok(()),
            }
        }
    }

    /// Holds the judge's live channel open, and says when it fires.
    ///
    /// Its own task, so this wait does not queue behind another. It only ever
    /// says *ask now*: the channel is lossy, so a verdict read from it is one
    /// that can be missed.
    async fn listening(&self, stopping: &Stopping) -> anyhow::Result<()> {
        loop {
            if stopping.now() {
                return Ok(());
            }

            // Registered before the count is read: `notify_waiters` stores
            // nothing for a task that is not yet waiting.
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();

            if self.outstanding() == 0 {
                tokio::select! {
                    _ = arrived => {}
                    _ = stopping.wait() => return Ok(()),
                }
                continue;
            }

            // Registered early, for the reason `arrived` is.
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();

            let told = tokio::select! {
                told = self.judge.wait_for_a_sign(Duration::from_secs(self.config.external.poll_max)) => told,
                // Nothing left to hear about: drop the channel now rather than
                // holding somebody else's request open for the rest of the
                // interval.
                _ = settled => false,
                _ = stopping.wait() => return Ok(()),
            };
            if told {
                self.sign.notify_waiters();
            }
        }
    }

    /// The stop handle, which `work` sets before anything can reach this.
    fn stop(&self) -> &Stopping {
        self.stopping
            .get()
            .expect("work sets the stop handle before any of this can run")
    }

    /// How many submissions the judge still owes an answer for.
    fn outstanding(&self) -> usize {
        self.pending.lock().expect("the pending lock").len()
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
    async fn give_everything_back(&self) {
        let held: Vec<(i64, String, String)> = self
            .pending
            .lock()
            .expect("the pending lock")
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
            self.pending.lock().expect("the pending lock").take(sid);
        }
    }

    /// How long until the judge is asked again.
    ///
    /// **Flat where the trigger is on**, because the stream is what makes a
    /// verdict prompt and this is what makes it certain. With it off the
    /// interval escalates instead, and freshness is traded against being a
    /// guest on somebody else's infrastructure.
    fn cycle(&self) -> Duration {
        let oldest = self
            .pending
            .lock()
            .expect("the pending lock")
            .iter()
            .map(|(_, entry)| entry.sent.elapsed())
            .max()
            .unwrap_or_default();
        crate::schedule::interval(
            self.config.external.long_poll_enabled,
            oldest,
            Duration::from_secs(self.config.external.poll_min),
            Duration::from_secs(self.config.external.poll_max),
            Duration::from_secs(self.config.external.poll_escalate_after),
        )
    }

    /// Everything that has to happen between claiming a job and waiting for it.
    async fn take(&self, job: ClaimedJob) {
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
                self.pending
                    .lock()
                    .expect("the pending lock")
                    .insert(entry.0, entry.1);
                // The two tasks that only have work while something is
                // outstanding are asleep until this.
                self.arrived.notify_waiters();
            }
            Err(Blocked::Verdict(refusal)) => {
                tracing::info!(job = %job.job_id, %refusal, "cannot be handed over as it stands");
                self.send(
                    &job.job_id,
                    &ReportResult::judged(&token, 0.0, 1.0, crate::integration::POLICY_VIOLATION),
                )
                .await;
            }
            Err(Blocked::Failure(why)) => {
                tracing::warn!(job = %job.job_id, %why, "not forwarded");
                self.fail(&job.job_id, &token, &why).await;
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
        &self,
        job: &ClaimedJob,
        setup: Setup,
        language: i64,
    ) -> Result<(i64, Entry), Blocked> {
        // **Where a live channel is, taken before the first of a batch leaves.**
        // Nothing is outstanding, so nothing can be lost by moving the
        // position — and the position goes stale while this Runner is idle,
        // because it stops listening when it has nothing to wait for.
        if self.outstanding() == 0 {
            self.judge.note_where_the_channel_is().await;
        }

        let pid = self
            .judge
            .problem(setup.number)
            .await
            .map_err(|e| Blocked::Failure(e.to_string()))?;

        let submitted = job
            .files
            .iter()
            .find(|f| f.name == "source")
            .or_else(|| job.files.first())
            .ok_or_else(|| Blocked::Failure("the submission carries no file".to_owned()))?;
        let held = self
            .cache
            .fetch(&self.server, &submitted.file_id, &submitted.sha256)
            .await
            .map_err(|e| Blocked::Failure(e.to_string()))?;

        // **A source that is not text is a verdict, not a failure of ours.**
        // What leaves this installation is the bytes of a form field, so a file
        // this cannot decode is one the judge could never have been given —
        // and calling that an infrastructure failure made it rejudgeable, which
        // meant every rejudge repeated it for ever against a file that will
        // never change.
        let bytes = std::fs::read(held.path()).map_err(|e| Blocked::Failure(e.to_string()))?;
        let source = String::from_utf8(bytes)
            .map_err(|_| Blocked::Verdict("the source file is not valid UTF-8 text".to_owned()))?;

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
            })
            .map_err(|e| Blocked::Failure(e.to_string()))?;

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
    async fn harvest(&self) {
        let outstanding: Vec<i64> = self
            .pending
            .lock()
            .expect("the pending lock")
            .sids()
            .collect();
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
            let entry = match self
                .pending
                .lock()
                .expect("the pending lock")
                .matched(id, self.judge.problem_of(answer))
            {
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

            if let Some(held) = self.pending.lock().expect("the pending lock").get_mut(id) {
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
            let Some(entry) = self.pending.lock().expect("the pending lock").take(sid) else {
                continue;
            };
            self.attach(&entry, document).await;
            self.send(&entry.job_id, &report).await;

            // **The loop closing is worth a line, because its absence reads as
            // a stall.** Handing over is logged and resolving was not, so a
            // healthy Runner and one wedged after submitting look identical in
            // the log — right up until the pending set ages out. Whoever is
            // watching should see the answer come back, and how long it took.
            tracing::info!(
                job = %entry.job_id,
                judge = self.judge.name(),
                sid,
                waited = ?entry.sent.elapsed(),
                "answered",
            );
        }
    }

    /// The judge did not answer in time.
    async fn expire(&self) {
        let timeout = Duration::from_secs(self.config.external.pending_timeout);
        // **Collected before the loop, so the lock is not held through it.**
        // The body reports to the Server, and a guard alive across that await
        // would stop every other task for the length of two HTTP calls each.
        let expired = {
            let pending = self.pending.lock().expect("the pending lock");
            pending.timed_out(timeout, Instant::now())
        };
        for sid in expired {
            let Some(entry) = self.pending.lock().expect("the pending lock").take(sid) else {
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
    async fn renew_everything(&self) {
        let held: Vec<(i64, String, String)> = self
            .pending
            .lock()
            .expect("the pending lock")
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
                .lock()
                .expect("the pending lock")
                .renewal(sid, !matches!(standing, Standing::Unreachable));

            match lease::act(standing, consecutive, ceiling) {
                Action::KeepWaiting => {}
                Action::DropSilently => {
                    tracing::warn!(job = %job_id, "the lease is gone; another Runner has this job");
                    self.pending.lock().expect("the pending lock").take(sid);
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
            let Some(entry) = self.pending.lock().expect("the pending lock").take(sid) else {
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
    ///
    /// **Or until the word comes.** Ten attempts backing off to thirty seconds
    /// is the better part of five minutes with no stop arm, against a grace of
    /// sixty — so a stop landing here was a `SIGKILL`, which drops this answer
    /// too and takes every other release with it. The lease requeues the job
    /// either way; the choice is only between losing it cleanly and losing more.
    async fn send(&self, job_id: &str, report: &ReportResult) {
        let mut backoff = Backoff::new(
            Duration::from_secs(self.config.claim_poll_min),
            Duration::from_secs(self.config.claim_poll_max),
        );
        // **Bounded by the lease, not by a count.** Ten attempts was the better
        // part of five minutes against a lease of twenty, so a Server that came
        // back at minute six found the answer already abandoned. What makes
        // giving up right is the lease expiring, because that is the moment
        // somebody else may legitimately take the job.
        let until = Instant::now() + Duration::from_secs(u64::from(self.config.lease_seconds));
        while Instant::now() < until {
            match self.server.report(job_id, report).await {
                Ok(accepted) => {
                    // **A repeat is not an error and says so.** Reporting is
                    // idempotent, and a Runner that retried into a Server which
                    // had already stored the answer should say which happened
                    // rather than leave two indistinguishable log lines.
                    if accepted.duplicate {
                        tracing::info!(
                            job = %job_id,
                            state = %accepted.state,
                            "the answer was already stored; this report changed nothing",
                        );
                    }
                    return;
                }
                Err(e) if e.lease_lost() => {
                    tracing::warn!(job = %job_id, "the lease was gone; the answer is dropped");
                    return;
                }
                // **A refusal is the Server having decided.** Asking again with
                // the same body gets the same answer, and the only thing the
                // repetition adds is a log nobody can act on.
                Err(e) if !e.retryable() && !e.unavailable() && !e.in_maintenance() => {
                    tracing::error!(%e, job = %job_id, "the Server refused the report");
                    return;
                }
                Err(e) => {
                    tracing::warn!(%e, job = %job_id, "the report did not land");
                    tokio::select! {
                        _ = backoff.wait() => {}
                        _ = self.stop().wait() => {
                            tracing::warn!(
                                job = %job_id,
                                "told to stop while carrying an answer; the lease will requeue the job",
                            );
                            return;
                        }
                    }
                }
            }
        }
    }
}

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

/// How often this Runner says it is alive.
///
/// **Not configurable, unlike the sandboxing Runner's.** That one is set per
/// fleet member because a fleet is many processes on one host; this is one
/// process against one judging system, and a number nobody would ever want to
/// change is better as a constant than as a key in `.env.example`.
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
