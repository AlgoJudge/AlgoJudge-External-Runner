//! How often to ask the archive anything.
//!
//! Two mechanisms, and they are not a pair of alternatives:
//!
//! - **The interval poll is the mechanism.** It runs unconditionally, at a flat
//!   sixty seconds, whether or not anything else is delivering. A safety net that
//!   only runs during an incident is exercised only during an incident, which is
//!   the worst moment to discover it is broken.
//! - **Long polling is a hint.** uHunt's event stream is global, buffers only the
//!   last hundred events, and its own documentation warns that a client that
//!   stops polling loses events. A missed event would mean a submission that
//!   never completes until its timeout — a correctness failure, not a latency
//!   one — so it triggers a fetch and is never read for verdicts.
//!
//! Escalation exists only for the case where the hint is switched off, because
//! that is the only case where freshness still has to be traded against being a
//! guest on somebody else's infrastructure.

use std::time::Duration;

/// How long to wait before asking again.
///
/// `since_oldest` is how long the oldest outstanding submission has been
/// waiting: escalation is a property of the wait, not of the wall clock.
pub fn interval(
    long_poll_enabled: bool,
    since_oldest: Duration,
    min: Duration,
    max: Duration,
    escalate_after: Duration,
) -> Duration {
    if long_poll_enabled {
        // Flat, and deliberately the slower of the two numbers. The trigger is
        // what makes a verdict prompt; this is what makes it certain.
        return max;
    }
    if since_oldest < escalate_after {
        min
    } else {
        max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(20);
    const MAX: Duration = Duration::from_secs(60);
    const ESCALATE: Duration = Duration::from_secs(120);

    /// With the accelerator on, the net is flat — one request a minute, not three.
    #[test]
    fn long_polling_makes_the_net_flat() {
        for waited in [0, 30, 119, 121, 600] {
            assert_eq!(
                interval(true, Duration::from_secs(waited), MIN, MAX, ESCALATE),
                MAX,
                "waited {waited}s"
            );
        }
    }

    /// With it off, the trade between freshness and politeness has to be made.
    #[test]
    fn without_it_the_interval_escalates_once() {
        assert_eq!(
            interval(false, Duration::from_secs(0), MIN, MAX, ESCALATE),
            MIN
        );
        assert_eq!(
            interval(false, Duration::from_secs(119), MIN, MAX, ESCALATE),
            MIN
        );
        assert_eq!(
            interval(false, Duration::from_secs(120), MIN, MAX, ESCALATE),
            MAX
        );
        assert_eq!(
            interval(false, Duration::from_secs(600), MIN, MAX, ESCALATE),
            MAX
        );
    }

    /// The net never falls below the floor either, whatever it is configured to.
    #[test]
    fn the_net_is_never_faster_than_the_floor() {
        let interval = interval(false, Duration::from_secs(0), MIN, MAX, ESCALATE);
        assert!(interval >= MIN, "{interval:?}");
    }
}
