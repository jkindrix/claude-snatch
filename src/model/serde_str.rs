//! Forward-compatible serde for string-valued enums.
//!
//! Claude Code occasionally introduces new values for string enum fields
//! (e.g. a new `stop_reason`). With a plain derived enum, an unrecognized value
//! is a hard deserialization error that drops the entire otherwise-valid entry
//! in lenient mode. [`serde_string_enum!`] generates `Serialize`/`Deserialize`
//! for an enum whose known variants map to fixed wire strings and whose
//! catch-all `Other(String)` variant captures any unrecognized value verbatim.
//!
//! Casing is preserved exactly: known variants serialize to their declared wire
//! string, and `Other` serializes the captured string unchanged (no
//! snake/camel/lowercase normalization).

/// Generate `Serialize`/`Deserialize` impls for a string-valued enum with a
/// verbatim `Other(String)` fallback.
///
/// The enum itself is declared normally (with its own derives and doc comments)
/// but must NOT derive `Serialize`/`Deserialize` and must include the named
/// catch-all variant carrying a `String`.
macro_rules! serde_string_enum {
    ($ty:ident { $($variant:ident => $wire:literal),+ $(,)? } other $other:ident) => {
        impl ::serde::Serialize for $ty {
            fn serialize<S>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                match self {
                    $( Self::$variant => serializer.serialize_str($wire), )+
                    Self::$other(value) => serializer.serialize_str(value),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let raw = ::std::string::String::deserialize(deserializer)?;
                ::core::result::Result::Ok(match raw.as_str() {
                    $( $wire => Self::$variant, )+
                    _ => Self::$other(raw),
                })
            }
        }
    };
}

pub(crate) use serde_string_enum;
