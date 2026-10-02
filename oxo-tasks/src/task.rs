/// The two independent kinds of work a tile needs.
///
/// They are separate tasks because they share no data, their resource
/// profiles differ by orders of magnitude, and their dependencies are
/// disjoint — an imagery provider and Overpass for ortho, an X-Plane
/// overlay source and DSFTool for overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum TaskType {
    Ortho,
    Overlay,
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TaskType {
    /// The canonical lowercase name, used as the database enum label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ortho => "ortho",
            Self::Overlay => "overlay",
        }
    }

    /// Parse the canonical form. Exact match only — no case folding, so a
    /// database label and this enum cannot drift apart silently.
    ///
    /// The reason these string forms exist at all: the PostgreSQL schema
    /// stores task type and state as `text` with a `CHECK` constraint rather
    /// than as PostgreSQL enums. A PostgreSQL enum would require
    /// `#[derive(sqlx::Type)]` on this enum, which would pull `sqlx` into
    /// `oxo-tasks` and break the crate split that lets a consumer depend on
    /// the port without a database driver. Do not "improve" this to a
    /// PostgreSQL enum without reading that decision first.
    pub fn from_str_exact(text: &str) -> Option<Self> {
        match text {
            "ortho" => Some(Self::Ortho),
            "overlay" => Some(Self::Overlay),
            _ => None,
        }
    }

    /// Every variant, for exhaustive iteration in tests and queries.
    pub const ALL: [TaskType; 2] = [TaskType::Ortho, TaskType::Overlay];
}

/// Where a task is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TaskState {
    /// Claimable once `claimable_at` has passed.
    Pending,
    /// Held under a lease, expected to heartbeat.
    Claimed,
    Succeeded,
    /// Retries exhausted. The job can never complete.
    Abandoned,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Succeeded => "succeeded",
            Self::Abandoned => "abandoned",
        }
    }

    /// Parse the canonical form. Exact match only — no case folding, so a
    /// database label and this enum cannot drift apart silently.
    ///
    /// The reason these string forms exist at all: the PostgreSQL schema
    /// stores task type and state as `text` with a `CHECK` constraint rather
    /// than as PostgreSQL enums. A PostgreSQL enum would require
    /// `#[derive(sqlx::Type)]` on this enum, which would pull `sqlx` into
    /// `oxo-tasks` and break the crate split that lets a consumer depend on
    /// the port without a database driver. Do not "improve" this to a
    /// PostgreSQL enum without reading that decision first.
    pub fn from_str_exact(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "succeeded" => Some(Self::Succeeded),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Abandoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_types_are_distinct_and_namable() {
        assert_ne!(TaskType::Ortho, TaskType::Overlay);
        assert_eq!(TaskType::Ortho.as_str(), "ortho");
        assert_eq!(TaskType::Overlay.as_str(), "overlay");
    }

    #[test]
    fn task_type_round_trips_through_its_string_form() {
        for task_type in [TaskType::Ortho, TaskType::Overlay] {
            assert_eq!(
                TaskType::from_str_exact(task_type.as_str()),
                Some(task_type)
            );
        }
        assert_eq!(TaskType::from_str_exact("ORTHO"), None);
        assert_eq!(TaskType::from_str_exact("mesh"), None);
    }

    #[test]
    fn terminal_states_are_marked_as_such() {
        assert!(!TaskState::Pending.is_terminal());
        assert!(!TaskState::Claimed.is_terminal());
        assert!(TaskState::Succeeded.is_terminal());
        assert!(TaskState::Abandoned.is_terminal());
    }

    #[test]
    fn task_state_round_trips_through_its_string_form() {
        for state in [
            TaskState::Pending,
            TaskState::Claimed,
            TaskState::Succeeded,
            TaskState::Abandoned,
        ] {
            assert_eq!(TaskState::from_str_exact(state.as_str()), Some(state));
        }
        assert_eq!(TaskState::from_str_exact("running"), None);
        // A case variant, specifically: this is the assertion that would
        // catch an accidental `.to_lowercase()` creeping into the parse.
        // TaskType's round-trip test pins the same rule; without this line
        // TaskState's exact-match guarantee is unasserted.
        assert_eq!(TaskState::from_str_exact("PENDING"), None);
    }
}
