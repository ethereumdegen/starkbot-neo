use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

id_type!(TaskId);
id_type!(MessageId);
id_type!(ConversationId);
id_type!(TurnId);
// `RunId` is one agent run: a user message, the steps it took, and how it
// ended. It is minted by the caller before the run starts, so a front end can
// subscribe and filter by it without a handshake. Not a `TurnId`: a run makes
// several model round trips, and each of those is a `Turn` row.
id_type!(RunId);
id_type!(ConfirmId);
id_type!(AskId);
id_type!(VerdictId);
id_type!(DisplayId);
id_type!(MediaJobId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_uuid_v7() {
        assert_eq!(TaskId::new().as_uuid().get_version_num(), 7);
    }

    #[test]
    fn ids_round_trip_through_strings() {
        let id = MessageId::new();
        let parsed = id
            .to_string()
            .parse::<MessageId>()
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(parsed, id);
    }
}
