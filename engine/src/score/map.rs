//! An insertion-ordered string map that serialises as a plain YAML/JSON mapping.
//!
//! Both the score format and the compiled contract use mappings whose order is meaningful:
//! `tracks:` defines the track order (and with it the index the engine addresses a track by), and
//! the per-section `tracks:` mapping is rendered in that same order in the block preview.
//! `BTreeMap` would sort it and `HashMap` would shuffle it, so this is a `Vec` of pairs with map
//! serialisation bolted on - which is all that is needed for a handful of entries.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

/// Hand-written so that an empty map exists for *any* value type - `#[derive(Default)]` would
/// demand `V: Default`, which none of the score's value types have or need.
impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Append or overwrite, keeping the position of an existing key.
    pub fn insert(&mut self, key: impl Into<String>, value: V) {
        let key = key.into();
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<V> FromIterator<(String, V)> for OrderedMap<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<V: Serialize> Serialize for OrderedMap<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for OrderedMap<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedMapVisitor<V>(PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for OrderedMapVisitor<V> {
            type Value = OrderedMap<V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("ein Mapping (Schluessel: Wert)")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut map = OrderedMap::new();
                while let Some((key, value)) = access.next_entry::<String, V>()? {
                    map.insert(key, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(OrderedMapVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_order_survives_a_round_trip() {
        let mut map = OrderedMap::new();
        map.insert("voice", 1u32);
        map.insert("gitarre", 2);
        map.insert("bass", 3);
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, r#"{"voice":1,"gitarre":2,"bass":3}"#);
        let back: OrderedMap<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.keys().collect::<Vec<_>>(), vec!["voice", "gitarre", "bass"]);
    }

    #[test]
    fn overwriting_keeps_the_original_position() {
        let mut map = OrderedMap::new();
        map.insert("a", 1u32);
        map.insert("b", 2);
        map.insert("a", 9);
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(map.get("a"), Some(&9));
    }
}
