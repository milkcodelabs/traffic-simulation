use serde::{Deserialize, Serialize};

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub u32);

        impl $name {
            pub const INVALID: Self = Self(u32::MAX);

            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }

            #[inline]
            pub fn is_valid(self) -> bool {
                self.0 != u32::MAX
            }
        }
    };
}

id_newtype!(NodeId);
id_newtype!(EdgeId);
id_newtype!(TurnId);
id_newtype!(SignalId);
id_newtype!(ZoneId);
