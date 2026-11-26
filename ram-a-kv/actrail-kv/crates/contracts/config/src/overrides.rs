//! Maps configuration fields to typed, sparse overrides without copying their defaults.
use serde::Serialize;

pub trait ConfigField {
    type Overrides: Serialize;
}

macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        impl ConfigField for $ty {
            type Overrides = Self;
        }
    )*};
}

leaf!(
    bool,
    String,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    std::path::PathBuf,
    std::num::NonZeroU32,
    std::num::NonZeroU64,
    std::num::NonZeroUsize,
);

impl<T: ConfigField> ConfigField for Option<T> {
    type Overrides = T::Overrides;
}

impl<T: Serialize> ConfigField for Vec<T> {
    type Overrides = Self;
}
