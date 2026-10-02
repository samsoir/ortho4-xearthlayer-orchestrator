//! Lease expiry is a correctness obligation of the control plane, not
//! optional maintenance — a worker that dies without this loop holds its
//! task forever. The loop lives in the library so its behaviour is
//! testable; the composition root only spawns it.

use std::sync::Arc;

use oxo_tasks::{ReapRequest, TaskStore};

/// Drive lease expiry forever: call `reap_expired` every `every`, log
/// passes that reclaimed anything, and keep going when a pass fails —
/// a transient database outage must not end lease enforcement.
pub async fn run(store: Arc<dyn TaskStore>, request: ReapRequest, every: std::time::Duration) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        match store.reap_expired(request).await {
            Ok(outcome) if outcome.requeued > 0 || outcome.abandoned > 0 => {
                tracing::info!(
                    requeued = outcome.requeued,
                    abandoned = outcome.abandoned,
                    "reaped expired leases"
                );
            }
            Ok(_) => {}
            Err(error) => {
                // Transient by assumption: the next tick retries. Ending
                // the loop here would silently disable retry-on-death.
                tracing::warn!(%error, "reap pass failed; will retry next interval");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use oxo_spec::TileId;
    use oxo_tasks::{
        BackoffSeconds, ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, FindJob,
        InMemoryTaskStore, JobCreated, JobId, JobStatus, Lease, MaxAttempts, ReapOutcome,
        ReapRequest, TaskSpec, TaskStore, TaskStoreError, TaskType, TestClock, Throughput,
        TimeoutSeconds,
    };

    use super::run;

    fn one_task_job() -> CreateJob {
        CreateJob {
            region_code: "NA".to_string(),
            revision: 1,
            max_attempts: MaxAttempts::new(3).expect("non-zero"),
            backoff: BackoffSeconds::new(60).expect("in range"),
            tasks: vec![TaskSpec {
                tile: TileId::new(50, -2).expect("in range"),
                task_type: TaskType::Ortho,
            }],
        }
    }

    fn request() -> ReapRequest {
        ReapRequest {
            heartbeat_timeout: TimeoutSeconds::new(60).expect("in range"),
            max_task_duration: TimeoutSeconds::new(3600).expect("in range"),
        }
    }

    fn any(worker: &str) -> ClaimRequest {
        ClaimRequest {
            worker: worker.to_string(),
            task_types: None,
        }
    }

    fn fixture() -> (Arc<InMemoryTaskStore>, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        ));
        (Arc::new(InMemoryTaskStore::new(clock.clone())), clock)
    }

    /// Let the spawned loop run whatever is due without waiting on real time.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    /// Fails `reap_expired` with an adapter error for the first `failures`
    /// calls, then delegates. Every other method delegates unconditionally.
    struct FlakyStore {
        inner: Arc<InMemoryTaskStore>,
        failures: u32,
        reap_calls: AtomicU32,
    }

    #[async_trait]
    impl TaskStore for FlakyStore {
        async fn create_job(&self, request: CreateJob) -> Result<JobCreated, TaskStoreError> {
            self.inner.create_job(request).await
        }
        async fn find_job(&self, request: FindJob) -> Result<Option<JobId>, TaskStoreError> {
            self.inner.find_job(request).await
        }
        async fn claim(
            &self,
            request: ClaimRequest,
        ) -> Result<Option<ClaimedTask>, TaskStoreError> {
            self.inner.claim(request).await
        }
        async fn heartbeat(&self, lease: Lease) -> Result<(), TaskStoreError> {
            self.inner.heartbeat(lease).await
        }
        async fn complete(&self, lease: Lease) -> Result<(), TaskStoreError> {
            self.inner.complete(lease).await
        }
        async fn fail(&self, request: FailRequest) -> Result<FailOutcome, TaskStoreError> {
            self.inner.fail(request).await
        }
        async fn reap_expired(&self, request: ReapRequest) -> Result<ReapOutcome, TaskStoreError> {
            let call = self.reap_calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                return Err(TaskStoreError::Adapter("down".to_string()));
            }
            self.inner.reap_expired(request).await
        }
        async fn job_status(&self, job_id: JobId) -> Result<JobStatus, TaskStoreError> {
            self.inner.job_status(job_id).await
        }
        async fn throughput(&self, job_id: JobId) -> Result<Throughput, TaskStoreError> {
            self.inner.throughput(job_id).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_reaper_reclaims_an_expired_lease_on_schedule() {
        let (store, clock) = fixture();
        store.create_job(one_task_job()).await.expect("create");
        let first = store.claim(any("pod-1")).await.unwrap().unwrap();
        assert_eq!(first.attempt, 1);

        let handle = tokio::spawn(run(store.clone(), request(), Duration::from_secs(30)));

        // Both clocks must move: the loop runs on tokio time, expiry is
        // judged by the store's clock.
        clock.advance(Duration::from_secs(61));
        tokio::time::advance(Duration::from_secs(90)).await;
        settle().await;

        // The reap applied the 60s backoff; let it elapse on the store clock.
        clock.advance(Duration::from_secs(61));
        let again = store
            .claim(any("pod-2"))
            .await
            .unwrap()
            .expect("the reaped task is claimable again");
        assert_eq!(again.attempt, 2);

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn an_adapter_error_does_not_end_the_loop() {
        let (inner, clock) = fixture();
        inner.create_job(one_task_job()).await.expect("create");
        inner.claim(any("pod-1")).await.unwrap().unwrap();
        let flaky = Arc::new(FlakyStore {
            inner: inner.clone(),
            failures: 3,
            reap_calls: AtomicU32::new(0),
        });

        let handle = tokio::spawn(run(flaky.clone(), request(), Duration::from_secs(30)));

        clock.advance(Duration::from_secs(61));
        // Step one interval at a time so each pass runs: three fail, a
        // later one must still reap.
        for _ in 0..6 {
            tokio::time::advance(Duration::from_secs(30)).await;
            settle().await;
        }
        assert!(
            flaky.reap_calls.load(Ordering::SeqCst) > 3,
            "the loop kept calling after errors"
        );
        assert!(!handle.is_finished(), "an error must not end the loop");

        clock.advance(Duration::from_secs(61));
        let again = inner
            .claim(any("pod-2"))
            .await
            .unwrap()
            .expect("a later pass reaped the lease");
        assert_eq!(again.attempt, 2);

        handle.abort();
    }
}
