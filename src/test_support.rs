use indexmap::IndexMap;

use crate::config::Extractor;
use crate::expr::Value;
use crate::render::FactStrings;

pub trait IntoFactValue {
    fn into_fact_value(self) -> Value;
}

impl IntoFactValue for Value {
    fn into_fact_value(self) -> Value {
        self
    }
}

impl IntoFactValue for &str {
    fn into_fact_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoFactValue for String {
    fn into_fact_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoFactValue for bool {
    fn into_fact_value(self) -> Value {
        Value::Bool(self)
    }
}

pub fn facts<V, const N: usize>(items: [(&str, V); N]) -> IndexMap<String, Value>
where
    V: IntoFactValue,
{
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.into_fact_value()))
        .collect()
}

pub fn no_facts() -> IndexMap<String, Value> {
    IndexMap::new()
}

pub fn fact_strings<V, const N: usize>(items: [(&str, V); N]) -> FactStrings
where
    V: Into<String>,
{
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.into()))
        .collect()
}

pub fn vars<V, const N: usize>(items: [(&str, V); N]) -> IndexMap<String, String>
where
    V: Into<String>,
{
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.into()))
        .collect()
}

pub fn extractors<const N: usize>(items: [(&str, Extractor); N]) -> IndexMap<String, Extractor> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}
