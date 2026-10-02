use std::fmt;

use uuid::Uuid;

// Three separate invocations, so these are three distinct nominal
// types: passing a TaskId where a JobId belongs will not compile.
// Enforced by the type system, not by a test.
macro_rules! identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a fresh, random identity.
            pub fn generate() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

identity!(
    JobId,
    "Identifies one job: one submission of one specification revision."
);
identity!(
    TaskId,
    "Identifies one task: one tile, one task type, within one job."
);
identity!(
    LeaseToken,
    "Proves the right to report on a claimed task. Minted fresh at every claim, so a worker whose task was reclaimed cannot report on it."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identities_are_distinct() {
        assert_ne!(JobId::generate(), JobId::generate());
        assert_ne!(TaskId::generate(), TaskId::generate());
        assert_ne!(LeaseToken::generate(), LeaseToken::generate());
    }

    #[test]
    fn an_identity_round_trips_through_its_uuid() {
        let id = TaskId::generate();
        assert_eq!(TaskId::from_uuid(id.as_uuid()), id);
    }

    #[test]
    fn an_identity_displays_as_its_uuid() {
        let id = JobId::generate();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    // There is deliberately no test asserting that the three identities
    // are distinct *types*. That is a compile-time property: the macro
    // emits three separate nominal structs, so passing a TaskId where a
    // JobId belongs does not compile, and `cargo build` already enforces
    // it. A `#[test]` wrapping a call that merely compiles asserts
    // nothing at runtime and can never fail.
}
