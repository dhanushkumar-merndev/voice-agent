//! Strongly typed UUIDv7 identifiers.
//!
//! UUIDv7 is time-ordered, so new rows land at the end of B-tree indexes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Creates a new time-ordered (UUIDv7) identifier.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }
    };
}

id_type!(
    /// A tenant (customer account).
    TenantId
);
id_type!(
    /// A knowledge base.
    KbId
);
id_type!(
    /// A document uploaded to a knowledge base.
    DocumentId
);
id_type!(
    /// A chunk of a knowledge-base document.
    ChunkId
);
id_type!(
    /// An outbound call.
    CallId
);
id_type!(
    /// A configured agent: character prompt, fields to collect, knowledge base.
    AgentId
);
id_type!(
    /// One turn (utterance) of a call transcript.
    TurnId
);
id_type!(
    /// A lesson an agent learned (or may learn) from a call.
    LessonId
);

/// The single tenant used until multi-tenancy lands. Seeded by the first migration.
pub const DEFAULT_TENANT: TenantId = TenantId(Uuid::from_u128(1));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_v7_and_ordered() {
        let a = CallId::new();
        let b = CallId::new();
        assert_eq!(a.0.get_version_num(), 7);
        assert!(a <= b);
    }

    #[test]
    fn round_trips_through_string_and_json() {
        let id = KbId::new();
        let parsed: KbId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
    }

    #[test]
    fn default_tenant_matches_migration_seed() {
        assert_eq!(
            DEFAULT_TENANT.to_string(),
            "00000000-0000-0000-0000-000000000001"
        );
    }
}
