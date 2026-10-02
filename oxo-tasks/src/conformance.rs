//! A contract every [`TaskStore`] adapter must satisfy.
//!
//! Behind the `conformance` feature, because this is test scaffolding
//! rather than API. The same cases run against the in-memory adapter and
//! the PostgreSQL one, which is what stops the in-memory adapter drifting
//! into a convenient fiction that passes while the real store would not.
//!
//! Cases assert invariants, never implementation. Use
//! [`conformance_suite!`] to generate one test per case.
//!
//! [`conformance_suite!`]: crate::conformance_suite

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use oxo_spec::TileId;

use crate::clock::TestClock;
use crate::error::TaskStoreError;
use crate::request::{
    ClaimRequest, ClaimedTask, CreateJob, FailOutcome, FailRequest, JobStatus, Lease, ReapRequest,
    TaskSpec,
};
use crate::store::TaskStore;
use crate::task::TaskType;

/// A freshly-made, empty store and the clock it reads.
pub struct Subject {
    pub store: Box<dyn TaskStore>,
    pub clock: Arc<TestClock>,
}

/// Produces a fresh, empty [`Subject`] for each case.
///
/// An adapter backed by a database must give each case genuine isolation
/// that is parallel-safe: its own schema, or its own database. Cargo runs
/// every case in this binary concurrently, so truncating shared tables in
/// `fresh()` races against every case still in flight — producing exactly
/// the baffling interference that isolation is here to prevent.
#[async_trait]
pub trait Fixture: Send + Sync {
    async fn fresh(&self) -> Subject;
}

fn tile(lat: i8, lon: i16) -> TileId {
    TileId::new(lat, lon).expect("in range")
}

/// Two tasks for one tile: the ortho build and its overlay.
pub fn two_task_job() -> CreateJob {
    CreateJob {
        region_code: "NA".to_string(),
        revision: 1,
        max_attempts: 3,
        backoff: Duration::from_secs(60),
        tasks: vec![
            TaskSpec {
                tile: tile(50, -2),
                task_type: TaskType::Ortho,
            },
            TaskSpec {
                tile: tile(50, -2),
                task_type: TaskType::Overlay,
            },
        ],
    }
}

/// One task, for a case that follows a single task through its whole life.
///
/// A two-task job derails such a case: claims come out oldest-claimable
/// first, so a requeued task sorts *behind* its never-claimed sibling and
/// the next claim hands back the sibling instead.
pub fn one_task_job() -> CreateJob {
    CreateJob {
        region_code: "NA".to_string(),
        revision: 1,
        max_attempts: 3,
        backoff: Duration::from_secs(60),
        tasks: vec![TaskSpec {
            tile: tile(50, -2),
            task_type: TaskType::Ortho,
        }],
    }
}

fn any_task(worker: &str) -> ClaimRequest {
    ClaimRequest {
        worker: worker.to_string(),
        task_types: None,
    }
}

fn lease_of(task: &ClaimedTask) -> Lease {
    Lease {
        task_id: task.task_id,
        token: task.lease,
    }
}

// ─── cases ───────────────────────────────────────────────────────────────

pub async fn creating_a_job_twice_resumes_it(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let first = subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let second = subject
        .store
        .create_job(two_task_job())
        .await
        .expect("resume");
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.job_id, second.job_id);
    assert_eq!(second.total_tasks, 2);
}

pub async fn a_changed_task_set_under_one_identity_conflicts(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let mut altered = two_task_job();
    altered.tasks.push(TaskSpec {
        tile: tile(51, -2),
        task_type: TaskType::Ortho,
    });
    let error = subject
        .store
        .create_job(altered)
        .await
        .expect_err("should conflict");
    assert!(
        matches!(error, TaskStoreError::JobConflict { .. }),
        "expected JobConflict, got {error:?}"
    );
}

pub async fn a_job_with_no_tasks_is_refused(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut empty = two_task_job();
    empty.tasks.clear();
    let error = subject
        .store
        .create_job(empty)
        .await
        .expect_err("should refuse");
    assert!(
        matches!(error, TaskStoreError::EmptyJob { .. }),
        "expected EmptyJob, got {error:?}"
    );
}

pub async fn a_job_with_a_duplicated_task_is_refused(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut doubled = two_task_job();
    doubled.tasks.push(doubled.tasks[0].clone());
    let error = subject
        .store
        .create_job(doubled)
        .await
        .expect_err("a duplicated task set must be refused, not deduplicated");
    assert!(
        matches!(error, TaskStoreError::DuplicateTask { .. }),
        "expected DuplicateTask, got {error:?}"
    );
}

pub async fn every_task_is_handed_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");

    let mut seen = Vec::new();
    while let Some(task) = subject.store.claim(any_task("pod")).await.expect("claim") {
        seen.push(task.task_id);
    }
    seen.sort();
    let unique = {
        let mut copy = seen.clone();
        copy.dedup();
        copy
    };
    assert_eq!(seen.len(), 2, "both tasks were handed out");
    assert_eq!(seen, unique, "no task was handed out twice");
}

pub async fn an_empty_queue_yields_none_not_an_error(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    assert!(subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
        .is_none());
}

pub async fn a_task_type_filter_is_honoured(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let claimed = subject
        .store
        .claim(ClaimRequest {
            worker: "pod".to_string(),
            task_types: Some(vec![TaskType::Overlay]),
        })
        .await
        .expect("claim")
        .expect("an overlay task exists");
    assert_eq!(claimed.task_type, TaskType::Overlay);

    let again = subject
        .store
        .claim(ClaimRequest {
            worker: "pod".to_string(),
            task_types: Some(vec![TaskType::Overlay]),
        })
        .await
        .expect("claim");
    assert!(
        again.is_none(),
        "only one overlay task exists, so a second filtered claim finds nothing — without \
         this the case passes whenever the untyped tie-break happens to pick the overlay"
    );
}

pub async fn a_stale_lease_is_refused_by_every_reporting_call(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
        .expect("a task");
    let stale = Lease {
        task_id: task.task_id,
        token: crate::ids::LeaseToken::generate(),
    };

    for error in [
        subject.store.heartbeat(stale).await.expect_err("heartbeat"),
        subject.store.complete(stale).await.expect_err("complete"),
        subject
            .store
            .fail(FailRequest {
                lease: stale,
                reason: "Crash!".to_string(),
            })
            .await
            .expect_err("fail"),
    ] {
        assert!(
            matches!(error, TaskStoreError::LeaseLost { .. }),
            "expected LeaseLost, got {error:?}"
        );
    }
}

/// The token a reclaimed worker still holds must be refused.
///
/// This is the at-most-once guarantee, and the case above cannot test it: a
/// freshly generated token is refused by an adapter that compares tokens
/// properly AND by one that stores no token at all. Only the *genuine
/// previous* token separates them. Get this wrong and two workers both
/// believe they own the task, so one silently clobbers the other's report.
pub async fn a_reclaimed_task_refuses_its_previous_holder(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    // One task, so the re-claim below cannot hand back a sibling.
    subject
        .store
        .create_job(one_task_job())
        .await
        .expect("create");

    let first = subject
        .store
        .claim(any_task("pod-a"))
        .await
        .expect("claim")
        .expect("a task");
    let abandoned = lease_of(&first);

    // The worker goes silent and the reaper takes the task back.
    subject.clock.advance(Duration::from_secs(120));
    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_task_duration: Duration::from_secs(86_400),
        })
        .await
        .expect("reap");
    assert_eq!(reaped.requeued, 1, "the silent worker's task is reclaimed");

    // A reaped task takes backoff, so wait it out, then let a second worker
    // take it. That worker now holds the only valid token.
    subject.clock.advance(Duration::from_secs(60));
    let second = subject
        .store
        .claim(any_task("pod-b"))
        .await
        .expect("claim")
        .expect("the reclaimed task is handed out again");
    assert_eq!(second.task_id, first.task_id, "the same task came back");
    assert_ne!(
        second.lease, first.lease,
        "a re-claim must mint a fresh token, or the old holder still has a live lease"
    );

    // The original holder is refused by all three reporting calls.
    for error in [
        subject
            .store
            .heartbeat(abandoned)
            .await
            .expect_err("heartbeat"),
        subject
            .store
            .complete(abandoned)
            .await
            .expect_err("complete"),
        subject
            .store
            .fail(FailRequest {
                lease: abandoned,
                reason: "Crash!".to_string(),
            })
            .await
            .expect_err("fail"),
    ] {
        assert!(
            matches!(error, TaskStoreError::LeaseLost { .. }),
            "the previous holder must be told its lease is lost, got {error:?}"
        );
    }

    // And the current holder is unaffected by that noise.
    subject
        .store
        .complete(lease_of(&second))
        .await
        .expect("the current holder can still report");
}

/// All three reporting calls agree on what an unknown task is.
///
/// An adapter that reports on a task with a single conditional write —
/// `WHERE id = $1 AND token = $2` — cannot tell "no such task" from "wrong
/// token" and will answer `LeaseLost` for both. Callers distinguish them:
/// one is a lost race to retry past, the other is a bug or a wiped store.
pub async fn an_unknown_task_is_refused_by_every_reporting_call(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let nowhere = Lease {
        task_id: crate::ids::TaskId::generate(),
        token: crate::ids::LeaseToken::generate(),
    };

    for error in [
        subject
            .store
            .heartbeat(nowhere)
            .await
            .expect_err("heartbeat"),
        subject.store.complete(nowhere).await.expect_err("complete"),
        subject
            .store
            .fail(FailRequest {
                lease: nowhere,
                reason: "Crash!".to_string(),
            })
            .await
            .expect_err("fail"),
    ] {
        assert!(
            matches!(error, TaskStoreError::UnknownTask { .. }),
            "expected UnknownTask, got {error:?}"
        );
    }
}

/// Several creators of one brand-new job must agree on the outcome.
///
/// Not hypothetical for OXO: dispatch is pull, so several pods can call
/// `create_job` for the same region and revision as they start. Exactly one
/// must create it and the rest must resume it, all naming the same job and
/// the same task count. A store that looks for the identity and only then
/// inserts cannot promise this -- `SELECT ... FOR UPDATE` locks rows that
/// exist, and a brand-new identity has none -- so every caller finds
/// nothing, all of them insert, and the losers get a constraint violation
/// where the contract says they should resume.
pub async fn concurrent_creation_of_one_job_happens_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let store: Arc<dyn TaskStore> = Arc::from(subject.store);

    let mut creators = Vec::new();
    for _ in 0..4 {
        let store = Arc::clone(&store);
        creators.push(tokio::spawn(async move {
            store.create_job(two_task_job()).await
        }));
    }

    let mut outcomes = Vec::new();
    for creator in creators {
        outcomes.push(
            creator
                .await
                .expect("creator did not panic")
                .expect("create_job either creates the job or resumes it, never fails"),
        );
    }

    let created = outcomes.iter().filter(|outcome| outcome.created).count();
    assert_eq!(created, 1, "exactly one caller created the job");

    let job_id = outcomes[0].job_id;
    for outcome in &outcomes {
        assert_eq!(outcome.job_id, job_id, "every caller names the same job");
        assert_eq!(
            outcome.total_tasks, 2,
            "every caller sees the whole task set, including the ones that resumed"
        );
    }
}

pub async fn a_failure_is_requeued_until_the_budget_is_spent(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_job();
    job.max_attempts = 2;
    subject.store.create_job(job).await.expect("create");

    let first = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    let requeued = subject
        .store
        .fail(FailRequest {
            lease: lease_of(&first),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");
    assert!(
        matches!(
            requeued,
            FailOutcome::Requeued {
                attempts_remaining: 1,
                ..
            }
        ),
        "expected one start remaining, got {requeued:?}"
    );

    subject.clock.advance(Duration::from_secs(60));
    let retry = loop {
        if let Some(task) = subject.store.claim(any_task("pod")).await.unwrap() {
            if task.task_id == first.task_id {
                break task;
            }
        } else {
            panic!("the requeued task never became claimable");
        }
    };
    assert_eq!(retry.attempt, 2);

    let abandoned = subject
        .store
        .fail(FailRequest {
            lease: lease_of(&retry),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");
    assert_eq!(abandoned, FailOutcome::Abandoned);
}

/// A requeued task waits out its backoff before it can be claimed again.
///
/// One task, so the claim below cannot be satisfied by a sibling. Without
/// this gate a tile that kills its worker is handed straight back and spends
/// its whole attempt budget in milliseconds.
pub async fn a_task_in_backoff_is_not_claimable_until_it_elapses(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(one_task_job())
        .await
        .expect("create");

    let claimed = subject
        .store
        .claim(any_task("pod"))
        .await
        .expect("claim")
        .expect("a task");
    let outcome = subject
        .store
        .fail(FailRequest {
            lease: lease_of(&claimed),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");
    assert!(
        matches!(outcome, FailOutcome::Requeued { .. }),
        "the budget allows another start, so this is a requeue: {outcome:?}"
    );

    assert!(
        subject
            .store
            .claim(any_task("pod"))
            .await
            .expect("claim")
            .is_none(),
        "a task still inside its backoff must not be handed out"
    );

    // one_task_job's backoff is sixty seconds.
    subject.clock.advance(Duration::from_secs(60));
    assert!(
        subject
            .store
            .claim(any_task("pod"))
            .await
            .expect("claim")
            .is_some(),
        "once the backoff has elapsed the task is claimable again"
    );
}

/// An empty capacity set claims nothing, rather than everything.
///
/// The PostgreSQL adapter's filter turns on the difference between an empty
/// `text[]` and SQL `NULL`, so the two readings of "no types" are one
/// mistake apart.
pub async fn an_empty_task_type_filter_claims_nothing(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");

    assert!(
        subject
            .store
            .claim(ClaimRequest {
                worker: "pod".to_string(),
                task_types: Some(Vec::new()),
            })
            .await
            .expect("claim")
            .is_none(),
        "a worker that can take no task type must be handed nothing"
    );
}

pub async fn a_lapsed_heartbeat_reclaims_the_task_and_spends_a_start(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_job();
    job.max_attempts = 1;
    subject.store.create_job(job).await.expect("create");

    subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject.clock.advance(Duration::from_secs(100));

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_task_duration: Duration::from_secs(86_400),
        })
        .await
        .expect("reap");
    assert_eq!(
        (reaped.requeued, reaped.abandoned),
        (0, 1),
        "the one permitted start was spent by claiming, so the reap abandons"
    );
}

pub async fn a_diligent_but_wedged_worker_is_cut_off_by_the_backstop(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");

    for _ in 0..10 {
        subject.clock.advance(Duration::from_secs(30));
        subject
            .store
            .heartbeat(lease_of(&task))
            .await
            .expect("a held lease heartbeats");
    }

    let reaped = subject
        .store
        .reap_expired(ReapRequest {
            heartbeat_timeout: Duration::from_secs(90),
            max_task_duration: Duration::from_secs(120),
        })
        .await
        .expect("reap");
    assert_eq!(reaped.requeued, 1);
}

pub async fn the_gate_moves_from_in_progress_to_complete(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let job = subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    assert!(matches!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::InProgress { pending: 2, .. }
    ));

    while let Some(task) = subject.store.claim(any_task("pod")).await.unwrap() {
        subject
            .store
            .complete(lease_of(&task))
            .await
            .expect("complete");
    }

    assert_eq!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::Complete
    );
}

pub async fn an_abandoned_task_is_visible_before_it_fails_the_job(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut spec = two_task_job();
    spec.max_attempts = 1;
    let job = subject.store.create_job(spec).await.expect("create");

    let doomed = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject
        .store
        .fail(FailRequest {
            lease: lease_of(&doomed),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");

    assert!(
        matches!(
            subject.store.job_status(job.job_id).await.expect("status"),
            JobStatus::InProgress { abandoned: 1, .. }
        ),
        "an unachievable run must be visible while other work continues"
    );

    let other = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject
        .store
        .complete(lease_of(&other))
        .await
        .expect("complete");

    assert_eq!(
        subject.store.job_status(job.job_id).await.expect("status"),
        JobStatus::Failed { abandoned: 1 }
    );
}

pub async fn an_unknown_job_is_refused_by_the_gate(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let error = subject
        .store
        .job_status(crate::ids::JobId::generate())
        .await
        .expect_err("unknown job");
    assert!(
        matches!(error, TaskStoreError::UnknownJob { .. }),
        "expected UnknownJob, got {error:?}"
    );
}

pub async fn throughput_separates_pending_from_claimable_now(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let job = subject
        .store
        .create_job(two_task_job())
        .await
        .expect("create");
    let task = subject
        .store
        .claim(any_task("pod"))
        .await
        .unwrap()
        .expect("a task");
    subject
        .store
        .fail(FailRequest {
            lease: lease_of(&task),
            reason: "Crash!".to_string(),
        })
        .await
        .expect("fail");

    let snapshot = subject
        .store
        .throughput(job.job_id)
        .await
        .expect("throughput");
    assert_eq!(snapshot.pending, 2);
    assert_eq!(snapshot.claimable_now, 1);

    subject.clock.advance(Duration::from_secs(60));
    let later = subject
        .store
        .throughput(job.job_id)
        .await
        .expect("throughput");
    assert_eq!(later.claimable_now, 2);
}

/// The invariant that matters most under a real database: concurrent
/// claimants must between them see each task exactly once.
pub async fn concurrent_claims_hand_each_task_out_exactly_once(fixture: &dyn Fixture) {
    let subject = fixture.fresh().await;
    let mut job = two_task_job();
    job.tasks = (0..24)
        .map(|n| TaskSpec {
            tile: tile(50, -24 + n),
            task_type: TaskType::Ortho,
        })
        .collect();
    let created = subject.store.create_job(job).await.expect("create");
    assert_eq!(created.total_tasks, 24);

    let store: Arc<dyn TaskStore> = Arc::from(subject.store);
    let mut workers = Vec::new();
    for worker in 0..8 {
        let store = Arc::clone(&store);
        workers.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            while let Some(task) = store
                .claim(ClaimRequest {
                    worker: format!("pod-{worker}"),
                    task_types: None,
                })
                .await
                .expect("claim")
            {
                mine.push(task.task_id);
            }
            mine
        }));
    }

    let mut all = Vec::new();
    for worker in workers {
        all.extend(worker.await.expect("worker did not panic"));
    }
    all.sort();
    let mut unique = all.clone();
    unique.dedup();

    assert_eq!(all.len(), 24, "every task was claimed");
    assert_eq!(all, unique, "no task was claimed twice");
}

/// Generate one `#[tokio::test]` per conformance case.
///
/// Takes an expression producing a [`Fixture`]. The case list lives here and
/// nowhere else, so adding a case covers every adapter without touching
/// their crates.
#[macro_export]
macro_rules! conformance_suite {
    ($fixture:expr) => {
        $crate::conformance_case!($fixture, creating_a_job_twice_resumes_it);
        $crate::conformance_case!($fixture, a_changed_task_set_under_one_identity_conflicts);
        $crate::conformance_case!($fixture, a_job_with_no_tasks_is_refused);
        $crate::conformance_case!($fixture, a_job_with_a_duplicated_task_is_refused);
        $crate::conformance_case!($fixture, concurrent_creation_of_one_job_happens_once);
        $crate::conformance_case!($fixture, every_task_is_handed_out_exactly_once);
        $crate::conformance_case!($fixture, an_empty_queue_yields_none_not_an_error);
        $crate::conformance_case!($fixture, a_task_type_filter_is_honoured);
        $crate::conformance_case!($fixture, a_stale_lease_is_refused_by_every_reporting_call);
        $crate::conformance_case!($fixture, a_reclaimed_task_refuses_its_previous_holder);
        $crate::conformance_case!($fixture, an_unknown_task_is_refused_by_every_reporting_call);
        $crate::conformance_case!($fixture, a_failure_is_requeued_until_the_budget_is_spent);
        $crate::conformance_case!(
            $fixture,
            a_task_in_backoff_is_not_claimable_until_it_elapses
        );
        $crate::conformance_case!($fixture, an_empty_task_type_filter_claims_nothing);
        $crate::conformance_case!(
            $fixture,
            a_lapsed_heartbeat_reclaims_the_task_and_spends_a_start
        );
        $crate::conformance_case!(
            $fixture,
            a_diligent_but_wedged_worker_is_cut_off_by_the_backstop
        );
        $crate::conformance_case!($fixture, the_gate_moves_from_in_progress_to_complete);
        $crate::conformance_case!(
            $fixture,
            an_abandoned_task_is_visible_before_it_fails_the_job
        );
        $crate::conformance_case!($fixture, an_unknown_job_is_refused_by_the_gate);
        $crate::conformance_case!($fixture, throughput_separates_pending_from_claimable_now);
        $crate::conformance_case!($fixture, concurrent_claims_hand_each_task_out_exactly_once);
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! conformance_case {
    ($fixture:expr, $case:ident) => {
        // The flavor is pinned, not incidental. A current-thread runtime
        // cannot interleave claimants against an adapter whose claim never
        // yields, so the concurrency case would pass while testing nothing.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $case() {
            let fixture = $fixture;
            $crate::conformance::$case(&fixture).await;
        }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_cases_are_registered() {
        let source = include_str!("conformance.rs");

        // Only scan the part before the test module to avoid matching test code
        let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
        let scannable = &source[..test_start];

        // Extract all case definitions: `pub async fn <name>(fixture: &dyn Fixture) {`
        let mut defined = Vec::new();
        let mut pos = 0;
        while let Some(idx) = scannable[pos..].find("pub async fn ") {
            let start = pos + idx + 13; // skip "pub async fn "
            if let Some(paren_pos) = scannable[start..].find('(') {
                let name = &scannable[start..start + paren_pos];
                if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    if let Some(param_end) =
                        scannable[start + paren_pos..].find("(fixture: &dyn Fixture)")
                    {
                        if param_end == 0 {
                            defined.push(name.to_string());
                        }
                    }
                }
                pos = start + paren_pos + 1;
            } else {
                pos = start + 1;
            }
        }

        // Extract all registered cases: look for conformance_case! invocations
        // Extract the name between $fixture, and the closing )
        let mut registered = Vec::new();
        let mut pos = 0;
        while let Some(idx) = scannable[pos..].find("conformance_case!(") {
            let start = pos + idx;
            // Find the position of $fixture, inside this invocation
            if let Some(fixture_pos) = scannable[start..].find("$fixture,") {
                let after_fixture = start + fixture_pos + 9; // skip "$fixture,"
                                                             // Find the closing paren for this invocation
                if let Some(close_paren) = scannable[after_fixture..].find(')') {
                    let name_raw = &scannable[after_fixture..after_fixture + close_paren];
                    let name = name_raw.trim();
                    if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        registered.push(name.to_string());
                    }
                    pos = after_fixture + close_paren + 1;
                } else {
                    pos = after_fixture + 1;
                }
            } else {
                pos = start + 18;
            }
        }

        // Sort for comparison
        defined.sort();
        registered.sort();

        // Check for mismatches
        let defined_set: std::collections::HashSet<_> = defined.iter().cloned().collect();
        let registered_set: std::collections::HashSet<_> = registered.iter().cloned().collect();

        let mut unregistered: Vec<_> = defined_set.difference(&registered_set).cloned().collect();
        let mut undefined: Vec<_> = registered_set.difference(&defined_set).cloned().collect();

        let mut errors = Vec::new();
        if !unregistered.is_empty() {
            unregistered.sort();
            errors.push(format!(
                "defined but unregistered cases: {}",
                unregistered.join(", ")
            ));
        }
        if !undefined.is_empty() {
            undefined.sort();
            errors.push(format!(
                "registered but undefined cases: {}",
                undefined.join(", ")
            ));
        }

        assert!(
            errors.is_empty(),
            "case registration mismatch: {}",
            errors.join("; ")
        );
    }
}
