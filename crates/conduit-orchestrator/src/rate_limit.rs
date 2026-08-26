#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLimitTracker {
    period: RateLimitPeriod,
    window_started_at: u64,
    now: u64,
    counters: RateLimitCounters,
}

impl RateLimitTracker {
    pub fn new(period: RateLimitPeriod) -> Self {
        Self {
            period,
            window_started_at: 0,
            now: 0,
            counters: RateLimitCounters::default(),
        }
    }

    pub fn with_period_ticks(period_ticks: u64) -> Self {
        Self::new(RateLimitPeriod::from_ticks(period_ticks))
    }

    pub const fn now(&self) -> u64 {
        self.now
    }

    pub const fn period(&self) -> RateLimitPeriod {
        self.period
    }

    pub const fn snapshot(&self) -> RateLimitSnapshot {
        RateLimitSnapshot {
            window_started_at: self.window_started_at,
            now: self.now,
            counters: self.counters,
        }
    }

    pub fn advance_by(&mut self, ticks: u64) {
        self.advance_to(self.now.saturating_add(ticks));
    }

    pub fn advance_to(&mut self, tick: u64) {
        self.now = tick;

        if self.now.saturating_sub(self.window_started_at) >= self.period.ticks {
            self.window_started_at = self.now;
            self.counters = RateLimitCounters::default();
        }
    }

    pub fn record_success(&mut self) {
        self.counters.successes = self.counters.successes.saturating_add(1);
    }

    pub fn record_failure(&mut self) {
        self.counters.failures = self.counters.failures.saturating_add(1);
    }

    pub fn record_usage_tokens(&mut self, tokens: u64) {
        self.counters.usage_tokens = self.counters.usage_tokens.saturating_add(tokens);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitPeriod {
    pub ticks: u64,
}

impl RateLimitPeriod {
    pub const DEFAULT_TICKS: u64 = 60;

    pub const fn from_ticks(ticks: u64) -> Self {
        if ticks == 0 {
            Self {
                ticks: Self::DEFAULT_TICKS,
            }
        } else {
            Self { ticks }
        }
    }
}

impl Default for RateLimitPeriod {
    fn default() -> Self {
        Self {
            ticks: Self::DEFAULT_TICKS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RateLimitCounters {
    pub successes: u64,
    pub failures: u64,
    pub usage_tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitSnapshot {
    pub window_started_at: u64,
    pub now: u64,
    pub counters: RateLimitCounters,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_tokens_are_accumulated() {
        let mut tracker = RateLimitTracker::with_period_ticks(60);

        tracker.record_usage_tokens(17);
        tracker.record_usage_tokens(25);

        assert_eq!(tracker.snapshot().counters.usage_tokens, 42);
    }

    #[test]
    fn success_and_failure_counts_are_recorded() {
        let mut tracker = RateLimitTracker::with_period_ticks(60);

        tracker.record_success();
        tracker.record_success();
        tracker.record_failure();

        assert_eq!(
            tracker.snapshot().counters,
            RateLimitCounters {
                successes: 2,
                failures: 1,
                usage_tokens: 0,
            }
        );
    }

    #[test]
    fn advancing_past_period_resets_window_counters() {
        let mut tracker = RateLimitTracker::with_period_ticks(10);
        tracker.record_success();
        tracker.record_usage_tokens(10);

        tracker.advance_to(10);

        assert_eq!(tracker.snapshot().window_started_at, 10);
        assert_eq!(tracker.snapshot().counters, RateLimitCounters::default());
    }
}
