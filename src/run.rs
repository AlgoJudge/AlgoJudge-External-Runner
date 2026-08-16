//! The loop: take work, hand it to the archive, wait, report.
//!
//! **One loop with two clocks**, rather than two tasks with a lock between them.
//! Asking our own Server for work is cheap and may be frequent; asking somebody
//! else's archive is neither, and is floored at twenty seconds. Keeping both in
//! one place means the pending set needs no synchronisation and the order of
//! operations is on the screen rather than in a scheduler.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aj_protocol::wire::{AttachToJob, ClaimedJob, Register, ReportResult};
use aj_protocol::{Backoff, Cache, Identity, Server};

use crate::config::Config;
use crate::lease::{self, Action, Standing};
use crate::pending::{Entry, Matched, Pending};
use crate::problem;
use crate::uva::site::{Refused, Site};
use crate::uva::uhunt::{self, Uhunt};
use crate::verdict::{self, Outcome};

/// Registered and holding a token, however long that takes.
pub async fn admitted(server: &Server, identity: &Identity, config: &Config) -> anyhow::Result<()> {
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(60));

    loop {
        let asked = server
            .register(&Register {
                name: config.runner_name.clone(),
                product: "algojudge-runner-uva".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                public_key: identity.public_key(),
                problem_types: config.problem_types.clone(),
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

pub struct Runner {
    pub server: Server,
    pub cache: Arc<Cache>,
    pub site: Site,
    pub uhunt: Uhunt,
    pub config: Config,
    pub uid: u64,
    pending: Pending,
    /// Public number to uHunt's internal id. Ours to re-derive, not to depend on.
    numbers: std::collections::BTreeMap<i64, i64>,
    /// Renewal attempts in a row that could not reach the Server.
    unreachable: u32,
}

impl Runner {
    pub fn new(
        server: Server,
        cache: Arc<Cache>,
        site: Site,
        uhunt: Uhunt,
        config: Config,
        uid: u64,
    ) -> Self {
        Self {
            server,
            cache,
            site,
            uhunt,
            config,
            uid,
            pending: Pending::default(),
            numbers: std::collections::BTreeMap::new(),
            unreachable: 0,
        }
    }

    pub async fn work(&mut self, identity: &Identity) -> anyhow::Result<()> {
        let mut claiming = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut ask_archive_at = Instant::now();
        let mut beat_at = Instant::now();

        loop {
            if !self.pending.is_empty() && Instant::now() >= ask_archive_at {
                self.renew_everything().await;
                self.harvest().await;
                self.expire().await;
                ask_archive_at = Instant::now() + self.cycle();
            }

            if self.pending.len() < self.config.max_pending {
                match self.server.claim(Some(self.config.lease_seconds)).await {
                    Ok(Some(job)) => {
                        claiming.reset();
                        self.take(job).await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) if e.needs_handshake() => {
                        self.server.forget_token();
                        admitted(&self.server, identity, &self.config).await?;
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
            // outstanding: wake in time for the archive, and no later.
            if self.pending.is_empty() {
                claiming.wait().await;
            } else {
                let until = ask_archive_at.saturating_duration_since(Instant::now());
                tokio::time::sleep(until.min(Duration::from_secs(5))).await;
            }
        }
    }

    /// How long until the archive is asked again.
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
            Duration::from_secs(self.config.poll_min),
            Duration::from_secs(self.config.poll_max),
            Duration::from_secs(self.config.poll_escalate_after),
        )
    }

    /// Everything that has to happen between claiming a job and waiting for it.
    async fn take(&mut self, job: ClaimedJob) {
        let token = job.lease_token.clone();
        match self.forward(&job).await {
            Ok(entry) => {
                if let Err(e) = self.server.progress(&job.job_id, &token).await {
                    tracing::warn!(%e, "could not say the work had started");
                }
                tracing::info!(job = %job.job_id, sid = entry.0, "handed to onlinejudge.org");
                self.pending.insert(entry.0, entry.1);
            }
            Err(why) => {
                tracing::warn!(job = %job.job_id, %why, "not forwarded");
                self.fail(&job.job_id, &token, &why.to_string()).await;
            }
        }
    }

    async fn forward(&mut self, job: &ClaimedJob) -> anyhow::Result<(i64, Entry)> {
        let setup = problem::read(job.config.as_ref())?;
        let language = setup.language(job.language.as_deref())?;

        let pid = match self.numbers.get(&setup.number) {
            Some(pid) => *pid,
            None => {
                let found = self.uhunt.problem(setup.number).await?;
                if found.status == 0 {
                    anyhow::bail!(
                        "onlinejudge.org lists problem {} as unavailable, so it cannot be judged",
                        setup.number
                    );
                }
                self.numbers.insert(setup.number, found.pid);
                found.pid
            }
        };

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

        let sid = self
            .site
            .submit(
                setup.number,
                language,
                &source,
                Duration::from_secs(self.config.submit_min_interval),
            )
            .await
            .map_err(|refused| match refused {
                Refused::SessionLapsed => anyhow::anyhow!("{}", Refused::SessionLapsed),
                Refused::Site(why) => anyhow::anyhow!("{}", Refused::Site(why)),
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
        let Some(after) = uhunt::cursor(self.pending.sids()) else {
            return;
        };
        let rows = match self.uhunt.since(self.uid, after).await {
            Ok(rows) => rows,
            // Not reaching the archive says nothing about anybody's solution.
            Err(e) => {
                tracing::warn!(%e, "could not read the archive");
                return;
            }
        };

        let mut done: Vec<(i64, Option<serde_json::Value>, ReportResult)> = Vec::new();
        for row in &rows {
            let entry = match self.pending.matched(row) {
                Matched::Stranger => continue,
                Matched::Disagrees { expected, found } => {
                    tracing::error!(
                        sid = row.sid,
                        expected,
                        found,
                        "a submission row names a different problem than the one we sent; \
                         not treating it as an answer"
                    );
                    continue;
                }
                Matched::Ours(entry) => entry.clone(),
            };

            if let Some(held) = self.pending.get_mut(row.sid) {
                held.trail.push(format!(
                    "row: [{},{},{},{},{},{}]",
                    row.sid,
                    row.pid,
                    row.verdict_id,
                    row.runtime_ms,
                    row.submitted_at,
                    row.language_id
                ));
            }

            match verdict::of(row.verdict_id) {
                Outcome::Pending => {}
                Outcome::Judged {
                    verdict,
                    abbreviation,
                } => {
                    let solved = verdict::solved(abbreviation, &entry.accepted);
                    let document =
                        crate::report::details(&entry, row, verdict, abbreviation, solved);
                    done.push((
                        row.sid,
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
                    let document = crate::report::details_of_failure(&entry, row.sid, reason);
                    done.push((
                        row.sid,
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

    /// The archive did not answer in time.
    async fn expire(&mut self) {
        let timeout = Duration::from_secs(self.config.pending_timeout);
        for sid in self.pending.timed_out(timeout, Instant::now()) {
            let Some(entry) = self.pending.take(sid) else {
                continue;
            };
            let why = format!(
                "onlinejudge.org did not judge submission {sid} within {} seconds",
                self.config.pending_timeout
            );
            let document = crate::report::details_of_failure(&entry, sid, &why);
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
        let ceiling = lease::ceiling(self.config.lease_seconds, self.config.poll_max);

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
            carried.push((
                "log",
                "text/plain",
                entry
                    .trail
                    .join(
                        "
",
                    )
                    .into_bytes(),
            ));
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
