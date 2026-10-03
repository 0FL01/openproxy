//! Request-local monotonic generation transport timing; retains only scalars.
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

tokio::task_local! {
    static GENERATION_TIMING: GenerationTiming;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GenerationTiming(Arc<Mutex<TimingState>>);

#[derive(Debug, Default)]
struct TimingState {
    started: Option<Instant>,
    prefix: Option<ReplayTiming>,
    prefix_unknown: bool,
}

/// Boundaries of the full original chunks saved by Codex preflight.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplayTiming {
    pub(crate) total_bytes: u64,
    pub(crate) last_chunk_start: u64,
    pub(crate) last_read_at: Instant,
}

impl GenerationTiming {
    /// Only the polled executor future inherits this observation, not spawned tasks.
    pub(crate) async fn scope<F: Future>(&self, future: F) -> F::Output {
        GENERATION_TIMING.scope(self.clone(), future).await
    }

    /// Unknown replay boundaries invalidate timing rather than resembling no replay.
    pub(crate) fn started(&self) -> Option<Instant> {
        let state = self.0.lock().ok()?;
        if state.prefix_unknown {
            None
        } else {
            state.started
        }
    }

    pub(crate) fn prefix(&self) -> Option<ReplayTiming> {
        self.0.lock().ok()?.prefix
    }
}

/// Call after preparation, immediately before the original generation send.
/// A replacement generation send starts afresh (e.g. endpoint escalation).
pub(crate) fn mark_generation_send() {
    let _ = GENERATION_TIMING.try_with(|timing| {
        let now = Instant::now();
        if let Ok(mut state) = timing.0.lock() {
            *state = TimingState {
                started: Some(now),
                ..Default::default()
            };
        }
    });
}

/// Call at the original successful prefix read, before copying or inspecting bytes.
pub(crate) fn mark_prefix_read(len: usize) {
    let _ = GENERATION_TIMING.try_with(|timing| {
        let now = Instant::now();
        if let Ok(mut state) = timing.0.lock() {
            state.record_prefix(len, now);
        }
    });
}

impl TimingState {
    fn record_prefix(&mut self, len: usize, now: Instant) {
        if self.started.is_none() || self.prefix_unknown || len == 0 {
            return;
        }
        let last_chunk_start = self.prefix.map_or(0, |prefix| prefix.total_bytes);
        let total_bytes = u64::try_from(len)
            .ok()
            .and_then(|len| last_chunk_start.checked_add(len));
        match total_bytes {
            Some(total_bytes) => {
                self.prefix = Some(ReplayTiming {
                    total_bytes,
                    last_chunk_start,
                    last_read_at: now,
                });
            }
            None => {
                self.prefix = None;
                self.prefix_unknown = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generation_timing_starts_only_at_scoped_send_and_shares_clones() {
        let timing = GenerationTiming::default();
        let clone = timing.clone();
        mark_generation_send();
        mark_prefix_read(12);
        assert!(timing.started().is_none());
        timing
            .scope(async {
                mark_prefix_read(12);
                assert!(timing.prefix().is_none());
                tokio::task::yield_now().await;
                assert!(timing.started().is_none());
                let before = Instant::now();
                mark_generation_send();
                let after = Instant::now();
                let started = clone.started().unwrap();
                assert!(before <= started && started <= after);
            })
            .await;
        let started = timing.started();
        mark_generation_send();
        assert_eq!(timing.started(), started);
    }

    #[tokio::test]
    async fn generation_timing_concurrent_scopes_are_isolated() {
        let first = GenerationTiming::default();
        let second = GenerationTiming::default();
        let barrier = tokio::sync::Barrier::new(2);
        tokio::join!(
            first.scope(async {
                mark_generation_send();
                mark_prefix_read(7);
                barrier.wait().await;
                assert_eq!(first.prefix().unwrap().total_bytes, 7);
            }),
            second.scope(async {
                barrier.wait().await;
                assert!(second.started().is_none());
                mark_generation_send();
                mark_prefix_read(11);
                assert_eq!(second.prefix().unwrap().total_bytes, 11);
            })
        );
        assert!(first.started().is_some());
        assert!(second.started().is_some());
    }

    #[tokio::test]
    async fn generation_timing_nested_scope_restores_parent_and_spawn_is_unscoped() {
        let outer = GenerationTiming::default();
        let inner = GenerationTiming::default();
        outer
            .scope(async {
                mark_generation_send();
                inner
                    .scope(async {
                        mark_generation_send();
                        mark_prefix_read(5);
                    })
                    .await;
                tokio::spawn(async { mark_prefix_read(100) }).await.unwrap();
                assert!(outer.prefix().is_none());
                mark_prefix_read(3);
            })
            .await;
        assert_eq!(outer.prefix().unwrap().total_bytes, 3);
        assert_eq!(inner.prefix().unwrap().total_bytes, 5);
    }

    #[tokio::test]
    async fn generation_timing_prefix_tracks_full_chunks_and_replacement_send_resets() {
        let timing = GenerationTiming::default();
        timing
            .scope(async {
                mark_generation_send();
                mark_prefix_read(4);
                let before = Instant::now();
                mark_prefix_read(100_000);
                let after = Instant::now();
                let prefix = timing.prefix().unwrap();
                assert_eq!(prefix.total_bytes, 100_004);
                assert_eq!(prefix.last_chunk_start, 4);
                assert!(before <= prefix.last_read_at && prefix.last_read_at <= after);
                mark_prefix_read(0);
                assert_eq!(timing.prefix().unwrap().last_read_at, prefix.last_read_at);
                let first_start = timing.started().unwrap();
                mark_generation_send();
                assert!(timing.started().unwrap() >= first_start);
                assert!(timing.prefix().is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn generation_timing_prefix_overflow_stays_unknown_until_next_send() {
        let timing = GenerationTiming::default();
        timing
            .scope(async {
                mark_generation_send();
                timing.0.lock().unwrap().prefix = Some(ReplayTiming {
                    total_bytes: u64::MAX,
                    last_chunk_start: 0,
                    last_read_at: Instant::now(),
                });
                mark_prefix_read(1);
                assert!(timing.started().is_none());
                assert!(timing.prefix().is_none());
                mark_prefix_read(1);
                assert!(timing.started().is_none());
                mark_generation_send();
                mark_prefix_read(1);
                assert!(timing.started().is_some());
                assert_eq!(timing.prefix().unwrap().total_bytes, 1);
            })
            .await;
    }

    #[tokio::test]
    async fn generation_timing_cancelled_scope_does_not_leak() {
        let timing = GenerationTiming::default();
        let scoped = timing.scope(async {
            mark_generation_send();
            std::future::pending::<()>().await;
        });
        let mut scoped = Box::pin(scoped);
        assert!(futures_util::poll!(&mut scoped).is_pending());
        drop(scoped);
        mark_prefix_read(8);
        assert!(timing.started().is_some());
        assert!(timing.prefix().is_none());
    }
}
