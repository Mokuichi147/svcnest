use crate::config::RestartPolicy;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const STABLE_WINDOW: Duration = Duration::from_secs(60);
pub const LIMIT_WINDOW: Duration = Duration::from_secs(300);
pub const RESTART_LIMIT: usize = 10;

#[derive(Default)]
pub struct Backoff {
    consecutive: u32,
    recent: VecDeque<Instant>,
}

impl Backoff {
    pub fn next(&mut self, now: Instant, uptime: Duration) -> Option<Duration> {
        if uptime >= STABLE_WINDOW {
            self.consecutive = 0;
        }
        while self
            .recent
            .front()
            .is_some_and(|time| now.duration_since(*time) >= LIMIT_WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= RESTART_LIMIT {
            return None;
        }
        self.recent.push_back(now);
        let seconds = (1u64 << self.consecutive.min(5)).min(30);
        self.consecutive = self.consecutive.saturating_add(1);
        Some(Duration::from_secs(seconds))
    }
}

pub fn should_restart(policy: RestartPolicy, success: bool, manual_stop: bool) -> bool {
    if manual_stop {
        return false;
    }
    match policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => !success,
        RestartPolicy::Always => true,
    }
}
