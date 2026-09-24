//! Adaptive distributed tracing sampling.
//!
//! A fixed sampling rate can overwhelm the tracing backend during traffic
//! spikes while under-sampling interesting events during quiet periods. This
//! module provides an adaptive sampler whose effective rate scales inversely
//! with the current request volume, while always sampling errors.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Configuration for the adaptive sampler.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveSamplingConfig {
    /// Sampling rate applied when traffic is at or below `low_volume_rps`.
    pub max_rate: f64,
    /// Sampling rate applied when traffic is at or above `high_volume_rps`.
    pub min_rate: f64,
    /// Request volume (requests per second) considered "low".
    pub low_volume_rps: f64,
    /// Request volume (requests per second) considered "high".
    pub high_volume_rps: f64,
    /// Window over which request volume is measured.
    pub window: Duration,
}

impl Default for AdaptiveSamplingConfig {
    fn default() -> Self {
        Self {
            max_rate: 1.0,
            min_rate: 0.01,
            low_volume_rps: 10.0,
            high_volume_rps: 1000.0,
            window: Duration::from_secs(1),
        }
    }
}

/// Adaptive sampler that tracks request volume and derives a sampling rate.
#[derive(Debug)]
pub struct AdaptiveSampler {
    config: AdaptiveSamplingConfig,
    window_start: Instant,
    window_requests: AtomicU64,
    /// Last computed rate, stored as bits for lock-free reads.
    last_rate_bits: AtomicU64,
}

impl AdaptiveSampler {
    pub fn new(config: AdaptiveSamplingConfig) -> Self {
        Self {
            last_rate_bits: AtomicU64::new(config.max_rate.to_bits()),
            config,
            window_start: Instant::now(),
            window_requests: AtomicU64::new(0),
        }
    }

    /// Records an incoming request and returns the current effective rate.
    pub fn record_request(&mut self) -> f64 {
        self.window_requests.fetch_add(1, Ordering::Relaxed);
        self.refresh_rate()
    }

    /// Returns the current effective sampling rate without recording a request.
    pub fn current_rate(&self) -> f64 {
        f64::from_bits(self.last_rate_bits.load(Ordering::Relaxed))
    }

    /// Recomputes the rate if the measurement window has elapsed.
    fn refresh_rate(&mut self) -> f64 {
        let elapsed = self.window_start.elapsed();
        if elapsed < self.config.window {
            return self.current_rate();
        }

        let requests = self.window_requests.swap(0, Ordering::Relaxed);
        self.window_start = Instant::now();
        let rps = requests as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
        let rate = self.rate_for_volume(rps);
        self.last_rate_bits.store(rate.to_bits(), Ordering::Relaxed);
        rate
    }

    /// Computes the sampling rate for a given request volume.
    ///
    /// The rate scales inversely with volume: it is `max_rate` at or below
    /// `low_volume_rps`, `min_rate` at or above `high_volume_rps`, and
    /// interpolated linearly in between.
    pub fn rate_for_volume(&self, rps: f64) -> f64 {
        let cfg = &self.config;
        if rps <= cfg.low_volume_rps {
            return cfg.max_rate;
        }
        if rps >= cfg.high_volume_rps {
            return cfg.min_rate;
        }
        let span = cfg.high_volume_rps - cfg.low_volume_rps;
        let position = (rps - cfg.low_volume_rps) / span;
        cfg.max_rate + (cfg.min_rate - cfg.max_rate) * position
    }

    /// Decides whether a request should be sampled.
    ///
    /// Errors are always sampled regardless of the adaptive rate.
    pub fn should_sample(&self, is_error: bool) -> bool {
        if is_error {
            return true;
        }
        let rate = self.current_rate();
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // Deterministic pseudo-random decision derived from the rate.
        let threshold = (rate * u64::MAX as f64) as u64;
        let sample = self.window_requests.load(Ordering::Relaxed);
        sample.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) < threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampler() -> AdaptiveSampler {
        AdaptiveSampler::new(AdaptiveSamplingConfig {
            max_rate: 1.0,
            min_rate: 0.01,
            low_volume_rps: 10.0,
            high_volume_rps: 1000.0,
            window: Duration::from_millis(1),
        })
    }

    #[test]
    fn rate_is_max_at_low_volume() {
        let s = sampler();
        assert_eq!(s.rate_for_volume(0.0), 1.0);
        assert_eq!(s.rate_for_volume(10.0), 1.0);
    }

    #[test]
    fn rate_is_min_at_high_volume() {
        let s = sampler();
        assert_eq!(s.rate_for_volume(1000.0), 0.01);
        assert_eq!(s.rate_for_volume(5000.0), 0.01);
    }

    #[test]
    fn rate_scales_inversely_with_volume() {
        let s = sampler();
        let low = s.rate_for_volume(100.0);
        let mid = s.rate_for_volume(500.0);
        let high = s.rate_for_volume(900.0);
        assert!(low > mid, "rate should decrease as volume grows");
        assert!(mid > high, "rate should decrease as volume grows");
    }

    #[test]
    fn errors_are_always_sampled() {
        let s = sampler();
        assert!(s.should_sample(true));
    }

    #[test]
    fn adaptive_rate_adjusts_under_simulated_load() {
        let mut s = sampler();
        // Simulate a burst of traffic within the window.
        for _ in 0..2000 {
            s.record_request();
        }
        std::thread::sleep(Duration::from_millis(5));
        let rate = s.record_request();
        assert!(rate < 1.0, "rate should drop under high load, got {rate}");
        assert!(rate >= 0.01, "rate should not drop below min, got {rate}");
    }
}
