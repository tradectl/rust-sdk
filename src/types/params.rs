use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Params {
    values: HashMap<String, f64>,
}

impl Params {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, key: &str, value: f64) -> Self {
        self.values.insert(key.to_string(), value);
        self
    }

    pub fn get(&self, key: &str, default: f64) -> f64 {
        self.values.get(key).copied().unwrap_or(default)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &f64)> {
        self.values.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.values.keys()
    }
}

/// Parameter definition for UI/validation/sweep ranges.
pub struct ParamDef {
    pub key: &'static str,
    pub description: &'static str,
    pub default: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
}
