-- Task type and state are text with a CHECK rather than PostgreSQL enums.
-- An enum would require sqlx::Type on the Rust enums, dragging sqlx into
-- oxo-tasks and breaking the dependency boundary the crate split exists to
-- maintain. The accepted labels match TaskType::as_str and TaskState::as_str
-- exactly, and from_str_exact does no case folding, so the two cannot drift
-- apart silently.

CREATE TABLE jobs (
    id           uuid        PRIMARY KEY,
    region_code  text        NOT NULL,
    -- A u32 in Rust. Without this CHECK an out-of-range revision casts to a
    -- negative i32 and inserts silently, corrupting the identity that the
    -- UNIQUE below is meant to protect. The other numeric columns are already
    -- covered by their own lower bounds.
    revision     bigint      NOT NULL CHECK (revision >= 0),
    -- Snapshotted from the specification's failure policy, so editing a
    -- specification cannot change the policy of a job already in flight.
    max_attempts bigint      NOT NULL CHECK (max_attempts >= 0),
    backoff_secs bigint      NOT NULL CHECK (backoff_secs >= 0),
    created_at   timestamptz NOT NULL,
    UNIQUE (region_code, revision)
);

CREATE TABLE tasks (
    id                uuid        PRIMARY KEY,
    job_id            uuid        NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    tile              text        NOT NULL,
    task_type          text        NOT NULL CHECK (task_type IN ('ortho', 'overlay')),
    state             text        NOT NULL CHECK (state IN ('pending', 'claimed', 'succeeded', 'abandoned')),
    -- Counts starts, not failures: incremented at claim.
    attempts          bigint      NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimable_at      timestamptz NOT NULL,
    lease_token       uuid,
    claimed_by        text,
    claimed_at        timestamptz,
    last_heartbeat_at timestamptz,
    last_failure      text,
    UNIQUE (job_id, tile, task_type),
    -- A claimed task holds a lease and a claim time; nothing else does.
    CONSTRAINT lease_matches_state CHECK (
        (state = 'claimed') = (lease_token IS NOT NULL)
        AND (state = 'claimed') = (claimed_at IS NOT NULL)
        AND (state = 'claimed') = (last_heartbeat_at IS NOT NULL)
    )
);

-- The claim query's hot path.
CREATE INDEX tasks_claimable ON tasks (claimable_at, id) WHERE state = 'pending';
-- The reaper's hot path.
CREATE INDEX tasks_claimed ON tasks (last_heartbeat_at) WHERE state = 'claimed';
