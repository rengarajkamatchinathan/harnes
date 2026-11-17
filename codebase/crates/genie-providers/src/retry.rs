//! Retry policy (03 §7; ported in design from OpenCode `model-retry`).

use std::time::Duration;

use crate::error::{ErrorKind, ProviderError, TransportPhase};

#[derive(Clone, Debug, PartialEq)]
pub struct RetryPolicy {
    pub base: Duration,
    pub max_gap: Duration,
    pub max_retries: u32,
    pub honor_retry_after_up_to: Duration,
    /// Network unreachable: separate budget.
    pub offline_budget: Duration,
    pub offline_max_gap: Duration,
    pub max_timeout_retries: u32,
    /// One-shot calls (titles, summaries, embeddings): only rate limits, 503/529 and
    /// not-sent transport errors.
    pub one_shot: bool,
    /// ±20 % jitter on computed gaps (off in tests).
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(2),
            max_gap: Duration::from_secs(10),
            max_retries: 10,
            honor_retry_after_up_to: Duration::from_secs(15 * 60),
            offline_budget: Duration::from_secs(60 * 60),
            offline_max_gap: Duration::from_secs(30),
            max_timeout_retries: 3,
            one_shot: false,
            jitter: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    Retry { after: Duration },
    GiveUp,
}

/// Counters the caller keeps across attempts of one request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetryState {
    /// Retries already made (all kinds).
    pub retries: u32,
    pub timeout_retries: u32,
    /// Time spent waiting on offline retries.
    pub offline_waited: Duration,
}

impl RetryPolicy {
    pub fn one_shot() -> Self {
        Self {
            one_shot: true,
            max_retries: 3,
            ..Self::default()
        }
    }

    /// Decide the next step for `err`. `output_started`: a failure after output is never
    /// retried here; the engine settles the partial part and starts a fresh message.
    pub fn decide(
        &self,
        err: &ProviderError,
        st: &RetryState,
        output_started: bool,
    ) -> RetryDecision {
        use RetryDecision::GiveUp;
        if output_started || err.kind == ErrorKind::Cancelled {
            return GiveUp;
        }
        if err.retry.forced == Some(false) {
            return GiveUp;
        }
        let offline = matches!(err.kind, ErrorKind::Transport { offline: true, .. });
        let eligible = if self.one_shot {
            matches!(
                err.kind,
                ErrorKind::RateLimited
                    | ErrorKind::Overloaded
                    | ErrorKind::Transport {
                        phase: TransportPhase::Connect | TransportPhase::Send,
                        ..
                    }
            ) || err.retry.forced == Some(true)
        } else {
            match err.kind {
                ErrorKind::RateLimited
                | ErrorKind::Overloaded
                | ErrorKind::ProviderInternal
                | ErrorKind::IncompleteStream
                | ErrorKind::Transport { .. } => true,
                ErrorKind::Timeout { .. } => st.timeout_retries < self.max_timeout_retries,
                _ => err.retry.forced == Some(true),
            }
        };
        if !eligible {
            return GiveUp;
        }
        if offline && !self.one_shot {
            if st.offline_waited >= self.offline_budget {
                return GiveUp;
            }
            let gap = self.backoff(st.retries, self.offline_max_gap);
            return RetryDecision::Retry {
                after: gap.min(self.offline_budget - st.offline_waited),
            };
        }
        if st.retries >= self.max_retries {
            return GiveUp;
        }
        if let Some(after) = err.retry.after {
            if after > self.honor_retry_after_up_to {
                return GiveUp;
            }
            return RetryDecision::Retry { after };
        }
        RetryDecision::Retry {
            after: self.backoff(st.retries, self.max_gap),
        }
    }

    /// `base * 2^n`, capped, with optional ±20 % jitter.
    pub fn backoff(&self, n: u32, cap: Duration) -> Duration {
        let exp = self.base.saturating_mul(1u32 << n.min(16));
        let gap = exp.min(cap);
        if !self.jitter {
            return gap;
        }
        let mut b = [0u8; 2];
        let r = if getrandom::fill(&mut b).is_ok() {
            u16::from_le_bytes(b) as f64 / u16::MAX as f64
        } else {
            0.5
        };
        gap.mul_f64(0.8 + 0.4 * r).min(cap)
    }

    /// Record a retry in `st`.
    pub fn note(&self, err: &ProviderError, after: Duration, st: &mut RetryState) {
        st.retries += 1;
        if matches!(err.kind, ErrorKind::Timeout { .. }) {
            st.timeout_retries += 1;
        }
        if matches!(err.kind, ErrorKind::Transport { offline: true, .. }) {
            st.offline_waited += after;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{RetryHint, TimeoutPhase};

    fn p() -> RetryPolicy {
        RetryPolicy {
            jitter: false,
            ..Default::default()
        }
    }
    fn e(kind: ErrorKind) -> ProviderError {
        ProviderError::new(kind, "x")
    }

    #[test]
    fn exponential_backoff_capped() {
        let p = p();
        let mut st = RetryState::default();
        let mut gaps = vec![];
        for _ in 0..4 {
            let RetryDecision::Retry { after } = p.decide(&e(ErrorKind::RateLimited), &st, false)
            else {
                panic!()
            };
            gaps.push(after.as_secs());
            p.note(&e(ErrorKind::RateLimited), after, &mut st);
        }
        assert_eq!(gaps, [2, 4, 8, 10]);
    }

    #[test]
    fn max_ten_retries() {
        let st = RetryState {
            retries: 10,
            ..Default::default()
        };
        assert_eq!(
            p().decide(&e(ErrorKind::Overloaded), &st, false),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn never_retried_kinds() {
        for k in [
            ErrorKind::QuotaExceeded,
            ErrorKind::Authentication,
            ErrorKind::InvalidRequest,
            ErrorKind::ContentPolicy { category: None },
            ErrorKind::Cancelled,
            ErrorKind::ContextOverflow,
        ] {
            assert_eq!(
                p().decide(&e(k.clone()), &RetryState::default(), false),
                RetryDecision::GiveUp,
                "{k:?}"
            );
        }
    }

    #[test]
    fn should_retry_header_wins() {
        let mut err = e(ErrorKind::Overloaded);
        err.retry.forced = Some(false);
        assert_eq!(
            p().decide(&err, &RetryState::default(), false),
            RetryDecision::GiveUp
        );
        let mut err = e(ErrorKind::InvalidRequest);
        err.retry = RetryHint {
            retryable: true,
            after: None,
            forced: Some(true),
        };
        assert!(matches!(
            p().decide(&err, &RetryState::default(), false),
            RetryDecision::Retry { .. }
        ));
    }

    #[test]
    fn retry_after_honoured_up_to_15_min() {
        let mut err = e(ErrorKind::RateLimited);
        err.retry.after = Some(Duration::from_secs(120));
        assert_eq!(
            p().decide(&err, &RetryState::default(), false),
            RetryDecision::Retry {
                after: Duration::from_secs(120)
            }
        );
        err.retry.after = Some(Duration::from_secs(16 * 60));
        assert_eq!(
            p().decide(&err, &RetryState::default(), false),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn timeouts_max_three() {
        let err = e(ErrorKind::Timeout {
            phase: TimeoutPhase::Headers,
        });
        let st = RetryState {
            retries: 3,
            timeout_retries: 3,
            ..Default::default()
        };
        assert_eq!(p().decide(&err, &st, false), RetryDecision::GiveUp);
    }

    #[test]
    fn offline_budget_separate() {
        let err = e(ErrorKind::Transport {
            phase: TransportPhase::Connect,
            offline: true,
        });
        let st = RetryState {
            retries: 50,
            ..Default::default()
        };
        assert_eq!(
            p().decide(&err, &st, false),
            RetryDecision::Retry {
                after: Duration::from_secs(30)
            }
        );
        let st = RetryState {
            retries: 50,
            offline_waited: Duration::from_secs(3600),
            ..Default::default()
        };
        assert_eq!(p().decide(&err, &st, false), RetryDecision::GiveUp);
    }

    #[test]
    fn output_started_never_retried() {
        assert_eq!(
            p().decide(&e(ErrorKind::Overloaded), &RetryState::default(), true),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn one_shot_policy() {
        let p = RetryPolicy {
            jitter: false,
            ..RetryPolicy::one_shot()
        };
        assert_eq!(
            p.decide(
                &e(ErrorKind::ProviderInternal),
                &RetryState::default(),
                false
            ),
            RetryDecision::GiveUp
        );
        assert!(matches!(
            p.decide(&e(ErrorKind::Overloaded), &RetryState::default(), false),
            RetryDecision::Retry { .. }
        ));
        let st = RetryState {
            retries: 3,
            ..Default::default()
        };
        assert_eq!(
            p.decide(&e(ErrorKind::RateLimited), &st, false),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let p = RetryPolicy::default();
        for n in 0..5 {
            let g = p.backoff(n, p.max_gap);
            assert!(g <= p.max_gap && g >= Duration::from_millis(1600), "{g:?}");
        }
    }
}
