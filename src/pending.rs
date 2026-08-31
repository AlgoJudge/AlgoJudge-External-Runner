//! What has been sent to a judge and not yet answered for.
//!
//! **Keyed on the external submission id and on nothing else.** Everything an
//! answer also carries — the problem, the language, the timestamp — is used to
//! *verify* a match, never to make one: an answer whose problem disagrees with
//! the entry it matched is a defect worth shouting about, not a match to accept
//! quietly.
//!
//! The set lives in memory. A restart loses it, which the Server handles by
//! reclaiming the lease and requeueing the job — correct, and it costs one extra
//! submission to somebody else's site, which is why the timeout is the shortest
//! thing that can be right rather than the most generous.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// One submission the archive owes us an answer for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub job_id: String,
    pub lease_token: String,
    /// The public number, for the message a person reads.
    pub problem_number: i64,
    /// The judge's **internal** id for the problem, for verifying an answer
    /// belongs to this entry.
    pub pid: i64,
    pub language_id: i64,
    /// When the archive accepted it. The timeout runs from here, not from the
    /// claim: waiting starts when the submission exists.
    pub sent: Instant,
    /// Whether the Server has been told the work is running.
    pub announced: bool,
    /// Which verdicts count as solved for **this** assignment, carried from the
    /// configuration chain so a later poll needs no second read of it.
    pub accepted: Vec<String>,
    /// What happened to this submission, in the order it happened.
    ///
    /// Appended to rather than summarised: when the verdict comes from somebody
    /// else's judge, the rows themselves are the only answer to a dispute about
    /// what that judge said.
    pub trail: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Pending {
    entries: BTreeMap<i64, Entry>,
}

/// What an answer turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Matched<'a> {
    /// Ours, and the answer agrees with what we sent.
    Ours(&'a Entry),
    /// Ours by id, but the answer describes a different problem.
    ///
    /// Not accepted as a match. The account is shared and ids are global, so the
    /// honest reading is that something is wrong with our own bookkeeping.
    Disagrees { expected: i64, found: i64 },
    /// Not ours. **Silence, not an error** — the account is shared, and the
    /// window necessarily holds our finished submissions and anybody else's.
    Stranger,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn insert(&mut self, sid: i64, entry: Entry) {
        self.entries.insert(sid, entry);
    }

    pub fn take(&mut self, sid: i64) -> Option<Entry> {
        self.entries.remove(&sid)
    }

    /// Every outstanding id, for the cursor.
    pub fn sids(&self) -> impl Iterator<Item = i64> + '_ {
        self.entries.keys().copied()
    }

    /// Every entry, for the lease renewal that has to reach all of them.
    pub fn iter(&self) -> impl Iterator<Item = (i64, &Entry)> {
        self.entries.iter().map(|(sid, entry)| (*sid, entry))
    }

    /// One entry, to append to its trail as the archive says more about it.
    ///
    /// Removed once as having no caller, and back because it has one: the
    /// evidence log is built while the rows arrive, not reconstructed after.
    pub fn get_mut(&mut self, sid: i64) -> Option<&mut Entry> {
        self.entries.get_mut(&sid)
    }

    /// What this answer is to us.
    ///
    /// **The two numbers, not the judge's whole answer.** Everything else an
    /// answer carries is the judge's own shape; these two are all a match is
    /// ever made or refused on, which is what keeps this type free of any
    /// particular judge.
    pub fn matched(&self, id: i64, problem: i64) -> Matched<'_> {
        match self.entries.get(&id) {
            None => Matched::Stranger,
            Some(entry) if entry.pid != problem => Matched::Disagrees {
                expected: entry.pid,
                found: problem,
            },
            Some(entry) => Matched::Ours(entry),
        }
    }

    /// Everything that has waited longer than it may.
    ///
    /// Returned rather than acted on here, because reporting needs the network
    /// and this type holds no client — and because a test can then drive the
    /// clock without driving a Server.
    pub fn timed_out(&self, timeout: Duration, now: Instant) -> Vec<i64> {
        self.entries
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.sent) >= timeout)
            .map(|(sid, _)| *sid)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: i64, sent: Instant) -> Entry {
        Entry {
            job_id: "job".into(),
            lease_token: "token".into(),
            problem_number: 100,
            pid,
            language_id: 1,
            sent,
            announced: false,
            accepted: vec!["AC".to_owned()],
            trail: Vec::new(),
        }
    }

    /// The account is shared. Somebody signing in by hand must not break a poll.
    #[test]
    fn a_row_we_did_not_send_is_not_ours_and_not_an_error() {
        let pending = Pending::default();
        assert_eq!(pending.matched(31254725, 36), Matched::Stranger);
    }

    #[test]
    fn a_row_we_sent_is_matched_by_its_id() {
        let mut pending = Pending::default();
        pending.insert(31254724, entry(36, Instant::now()));
        assert!(matches!(pending.matched(31254724, 36), Matched::Ours(_)));
    }

    /// The one case that is neither ours nor a stranger, and it means a defect.
    #[test]
    fn a_row_whose_problem_disagrees_is_not_accepted_as_a_match() {
        let mut pending = Pending::default();
        pending.insert(31254724, entry(36, Instant::now()));
        assert_eq!(
            pending.matched(31254724, 4838),
            Matched::Disagrees {
                expected: 36,
                found: 4838
            }
        );
    }

    #[test]
    fn the_timeout_runs_from_when_the_archive_accepted_it() {
        let now = Instant::now();
        let mut pending = Pending::default();
        pending.insert(1, entry(36, now - Duration::from_secs(901)));
        pending.insert(2, entry(36, now - Duration::from_secs(60)));

        let late = pending.timed_out(Duration::from_secs(900), now);
        assert_eq!(late, vec![1], "only the one past its deadline");
    }

    #[test]
    fn an_empty_set_has_nothing_outstanding_and_nothing_late() {
        let pending = Pending::default();
        assert!(pending.is_empty());
        assert_eq!(pending.sids().next(), None);
        assert!(pending
            .timed_out(Duration::from_secs(900), Instant::now())
            .is_empty());
    }

    /// The outstanding ids are read off the set rather than kept beside it,
    /// which is what lets a judge derive its own cursor from them.
    #[test]
    fn the_outstanding_ids_follow_the_set() {
        let now = Instant::now();
        let mut pending = Pending::default();
        pending.insert(300, entry(36, now));
        pending.insert(100, entry(36, now));
        assert_eq!(pending.sids().collect::<Vec<_>>(), vec![100, 300]);

        pending.take(100);
        assert_eq!(pending.sids().collect::<Vec<_>>(), vec![300]);
    }
}
