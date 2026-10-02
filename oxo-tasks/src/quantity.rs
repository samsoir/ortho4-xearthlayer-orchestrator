//! Checked whole-second and attempt quantities for the request surface.
//!
//! These exist to remove a class of defect: `std::time::Duration` on the
//! port forced every adapter to convert with
//! `chrono::Duration::from_std(…).unwrap_or(zero())`, whose failure case
//! inverted the caller's intent — an enormous timeout became "reclaim
//! everything now". A value that can be constructed here is representable
//! by every adapter, so the conversion sites are infallible and the
//! refusal lives in exactly one place.

use std::num::NonZeroU32;

use thiserror::Error;

/// Upper bound on any whole-second quantity the port accepts.
///
/// `chrono::Duration` counts milliseconds in an `i64`, so any seconds
/// value at or below this converts infallibly in every adapter. The bound
/// is representability, not policy — it is roughly 292 million years.
pub const MAX_SECONDS: u64 = (i64::MAX / 1_000) as u64;

/// Why a quantity was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InvalidQuantity {
    #[error("must be at least 1")]
    Zero,
    #[error("{0} seconds exceeds what every adapter can represent ({MAX_SECONDS})")]
    TooManySeconds(u64),
}

/// Delay before a failed or reaped task becomes claimable again.
///
/// Zero is legal and means "retry immediately": `oxo-spec` defaults
/// `backoff_seconds` to 0, so refusing it would make the default
/// specification unsubmittable. This deliberately narrows the settled
/// answer in the job-server design, which said "refuses zero" without
/// accounting for that default; the design document records the ruling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackoffSeconds(u64);

impl BackoffSeconds {
    pub const ZERO: Self = Self(0);

    pub fn new(seconds: u64) -> Result<Self, InvalidQuantity> {
        if seconds > MAX_SECONDS {
            return Err(InvalidQuantity::TooManySeconds(seconds));
        }
        Ok(Self(seconds))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// An expiry bound for the reaper: heartbeat timeout or maximum task
/// duration. Zero is refused — a zero bound reclaims every in-flight task
/// on the next tick, which is never what an operator meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeoutSeconds(u64);

impl TimeoutSeconds {
    pub fn new(seconds: u64) -> Result<Self, InvalidQuantity> {
        if seconds == 0 {
            return Err(InvalidQuantity::Zero);
        }
        if seconds > MAX_SECONDS {
            return Err(InvalidQuantity::TooManySeconds(seconds));
        }
        Ok(Self(seconds))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Total attempts allowed per task, including the first. At least 1:
/// zero is an inconsistent state — `claim` still hands the task out, so a
/// worker does hours of production on a budget that was already spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaxAttempts(NonZeroU32);

impl MaxAttempts {
    pub fn new(attempts: u32) -> Result<Self, InvalidQuantity> {
        NonZeroU32::new(attempts)
            .map(Self)
            .ok_or(InvalidQuantity::Zero)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_backoff_is_legal_and_means_retry_immediately() {
        // oxo-spec defaults backoff_seconds to 0; refusing it here would
        // make the default specification unsubmittable.
        assert_eq!(BackoffSeconds::new(0), Ok(BackoffSeconds::ZERO));
        assert_eq!(BackoffSeconds::ZERO.get(), 0);
    }

    #[test]
    fn a_zero_timeout_is_refused() {
        // A zero heartbeat timeout or max duration reclaims every in-flight
        // task on the next reap tick, which is never what an operator meant.
        assert_eq!(TimeoutSeconds::new(0), Err(InvalidQuantity::Zero));
    }

    #[test]
    fn seconds_beyond_what_every_adapter_can_represent_are_refused() {
        assert!(BackoffSeconds::new(MAX_SECONDS).is_ok());
        assert_eq!(
            BackoffSeconds::new(MAX_SECONDS + 1),
            Err(InvalidQuantity::TooManySeconds(MAX_SECONDS + 1))
        );
        assert!(TimeoutSeconds::new(MAX_SECONDS).is_ok());
        assert_eq!(
            TimeoutSeconds::new(MAX_SECONDS + 1),
            Err(InvalidQuantity::TooManySeconds(MAX_SECONDS + 1))
        );
    }

    #[test]
    fn the_bound_converts_infallibly_into_chrono_arithmetic() {
        // The whole point of MAX_SECONDS: chrono::Duration counts
        // milliseconds in an i64, so any accepted value converts without
        // the unwrap_or(zero) inversion this type exists to remove.
        let seconds = BackoffSeconds::new(MAX_SECONDS).expect("in range").get();
        let converted =
            chrono::Duration::try_seconds(i64::try_from(seconds).expect("bounded by construction"));
        assert!(converted.is_some());
    }

    #[test]
    fn zero_attempts_is_refused_because_it_is_an_inconsistent_state() {
        // max_attempts = 0 would still be claimable (attempts increment at
        // claim), so a worker does hours of work on a budget already spent.
        assert_eq!(MaxAttempts::new(0), Err(InvalidQuantity::Zero));
        assert_eq!(MaxAttempts::new(1).map(MaxAttempts::get), Ok(1));
        assert_eq!(
            MaxAttempts::new(u32::MAX).map(MaxAttempts::get),
            Ok(u32::MAX)
        );
    }

    #[test]
    fn refusals_render_something_an_operator_can_act_on() {
        assert!(InvalidQuantity::Zero.to_string().contains("at least 1"));
        let rendered = InvalidQuantity::TooManySeconds(MAX_SECONDS + 1).to_string();
        assert!(
            rendered.contains(&(MAX_SECONDS + 1).to_string()),
            "{rendered}"
        );
        assert!(rendered.contains(&MAX_SECONDS.to_string()), "{rendered}");
    }
}
