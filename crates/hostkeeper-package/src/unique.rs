//! Finding a repeated object key in a JSON text. `serde_json::Value` keeps the last of two equal
//! keys and forgets the first, so a reader that wants every key once has to look at the text
//! itself: [`repeated_key`] walks the document and names the first key an object repeats.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};

/// The walk over one JSON value. It holds where to put the first repeated key it meets.
#[derive(Clone, Copy)]
struct Walk<'a> {
    found: &'a Cell<Option<String>>,
}

impl<'de> DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Walk<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: Error>(self, _value: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: Error>(self, _value: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: Error>(self, _value: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: Error>(self, _value: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: Error>(self, _value: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(self)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if seen.contains(&key) {
                self.found.set(Some(key));
                return Err(A::Error::custom("a repeated key"));
            }
            seen.insert(key);
            map.next_value_seed(self)?;
        }
        Ok(())
    }
}

/// The first key that an object of `bytes` (top level or nested) holds twice, in document order
/// of the second occurrence. None when every object's keys are distinct, and also when `bytes`
/// is not JSON before any repeat: whether it is JSON is the caller's own parse to decide.
pub(crate) fn repeated_key(bytes: &[u8]) -> Option<String> {
    let found = Cell::new(None);
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let _walked = Walk { found: &found }.deserialize(&mut reader);
    found.into_inner()
}

#[cfg(test)]
#[path = "unique_tests.rs"]
mod tests;
