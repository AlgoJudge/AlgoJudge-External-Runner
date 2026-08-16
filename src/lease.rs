//! Keeping a job while somebody else's judge thinks about it.
//!
//! **`AlgoJudge-Runner` has no equivalent of this.** `Server::renew` exists in
//! `aj-protocol` and is called from the conformance suite and from nowhere else;
//! the production loop asks for ten minutes and never renews, because a local
//! evaluation finishes inside one. This Runner waits up to fifteen minutes on an
//! archive it does not control, so a job left on a lease it never extends is
//! reclaimed by the Server, handed to the next Runner, and **submitted to
//! onlinejudge.org a second time**. That is the failure this module exists for.
//!
//! The policy is deliberately dull: **renew every held job on every poll cycle,
//! unconditionally.** Renewal never shortens a lease — the Server's conformance
//! suite pins that — so there is no deadline arithmetic to get wrong, and no
//! second opinion about when a lease expires. The cycle is at most sixty seconds
//! against a lease of twenty minutes, which leaves nineteen failed renewals of
//! slack before anything is at risk.

/// What the Server's answer to a renewal means for the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Renewed. The authoritative expiry came back in the answer.
    Held,
    /// Somebody else holds this job now — the lease was reaped, or it was never
    /// ours. **Nothing may be reported for it.**
    Lost,
    /// The Server could not be reached or answered a server error. Says nothing
    /// about whether we still hold the job.
    Unreachable,
}

impl Standing {
    /// Read from the protocol's own classifiers rather than from status codes,
    /// so a contract change lands in one place.
    pub fn of(error: &aj_protocol::Error) -> Self {
        if error.lease_lost() {
            Self::Lost
        } else {
            Self::Unreachable
        }
    }
}

/// What to do about a held submission after a renewal attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Carry on waiting for the archive.
    KeepWaiting,
    /// Forget it **without reporting anything**.
    ///
    /// The job belongs to another Runner now. Pushing a result here would
    /// overwrite a newer attempt with an older answer, and the Server's
    /// idempotency would not save us: it keys on the lease token, and ours is
    /// the stale one.
    DropSilently,
    /// Stop holding it and say why. The Server requeues it.
    GiveUp,
}

/// The decision, given what the Server said and how long it has been saying it.
///
/// `consecutive` counts renewal attempts that could not reach the Server,
/// including this one. The ceiling is expressed in **attempts rather than
/// seconds** because the poll cycle is what drives them: it is the same clock
/// the waiting runs on, so the two cannot drift apart.
pub fn act(standing: Standing, consecutive: u32, ceiling: u32) -> Action {
    match standing {
        Standing::Held => Action::KeepWaiting,
        Standing::Lost => Action::DropSilently,
        Standing::Unreachable if consecutive >= ceiling => Action::GiveUp,
        Standing::Unreachable => Action::KeepWaiting,
    }
}

/// How many failed renewals a lease can absorb before it is genuinely at risk.
///
/// Derived rather than chosen: with a lease of `lease_seconds` and a cycle of at
/// most `cycle_seconds`, this is how many cycles fit — minus one, so the ceiling
/// is reached while the lease is still valid rather than after it has gone.
pub fn ceiling(lease_seconds: u32, cycle_seconds: u64) -> u32 {
    let cycle = cycle_seconds.max(1);
    ((u64::from(lease_seconds) / cycle).saturating_sub(1)).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_renewed_lease_just_carries_on() {
        assert_eq!(act(Standing::Held, 0, 10), Action::KeepWaiting);
    }

    /// The rule that keeps two Runners from overwriting one another.
    #[test]
    fn a_lost_lease_is_dropped_without_reporting() {
        assert_eq!(act(Standing::Lost, 0, 10), Action::DropSilently);
        // However long we had been failing beforehand: losing the job settles it.
        assert_eq!(act(Standing::Lost, 99, 10), Action::DropSilently);
    }

    /// An unreachable Server is not a lost job, and not a verdict either.
    #[test]
    fn an_unreachable_server_is_waited_out_then_given_up() {
        assert_eq!(act(Standing::Unreachable, 1, 5), Action::KeepWaiting);
        assert_eq!(act(Standing::Unreachable, 4, 5), Action::KeepWaiting);
        assert_eq!(act(Standing::Unreachable, 5, 5), Action::GiveUp);
    }

    /// The ceiling is reached while the lease is still valid, not after.
    #[test]
    fn the_ceiling_leaves_the_lease_still_alive() {
        // Twenty minutes of lease, a minute a cycle: nineteen, not twenty.
        assert_eq!(ceiling(1200, 60), 19);
        // A shorter cycle absorbs more failures.
        assert_eq!(ceiling(1200, 20), 59);
    }

    /// Configuration cannot produce a ceiling of zero, which would give up on
    /// the first failed renewal.
    #[test]
    fn the_ceiling_is_never_zero() {
        assert_eq!(ceiling(60, 60), 1);
        assert_eq!(ceiling(1, 3600), 1);
        assert_eq!(ceiling(1200, 0), 1199);
    }
}
