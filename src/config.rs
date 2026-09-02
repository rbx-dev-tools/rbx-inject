//! The injection rules: what to change, and where.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A whole injections file. Unknown fields are ignored on purpose: rbx-deck's
/// `injections.json` also carries a `webAssets` map that belongs to the asphalt
/// step, not to this one, and a config should not have to be split in two to be
/// readable by both.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub injections: Vec<Injection>,
}

/// One rule, targeting one instance.
#[derive(Debug, Deserialize, Serialize)]
pub struct Injection {
    /// Dot-separated path from the DataModel, e.g. `ReplicatedStorage.Assets`.
    #[serde(alias = "robloxPath", alias = "path")]
    pub roblox_path: String,

    /// Properties to set on that instance, `{ "Image": "ui.ShopIcon" }`.
    ///
    /// `BTreeMap`, not `HashMap`: the log lines and the `--dry-run` output are
    /// meant to be diffed between runs, so the order has to come from the keys
    /// rather than from a hash seed that changes every process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,

    /// Keys to set inside the table a ModuleScript returns,
    /// `{ "Sounds.Bang": "audio.Bang" }`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
}

impl Config {
    /// Load from a `.json` or `.toml` file, picked by extension.
    ///
    /// Both, because the config this replaces is JSON and there is no reason to
    /// make a working setup migrate before it can use a new binary. New projects
    /// should reach for TOML, which takes comments.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;

        let is_toml = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"));

        if is_toml {
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        } else {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
    }

    /// Rules that would change something. A rule with neither `properties` nor
    /// `keys` is a leftover, not an error.
    pub fn active(&self) -> impl Iterator<Item = &Injection> {
        self.injections
            .iter()
            .filter(|i| !i.properties.is_empty() || !i.keys.is_empty())
    }

    /// Render as TOML, for `rbx-inject migrate`.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("rendering TOML")
    }

    /// What this config needs to be applied at all.
    ///
    /// Checked before the place is touched, so that a forgotten `--assets`
    /// fails immediately and by name, instead of resolving nothing and looking
    /// exactly like a place that has drifted away from its rules.
    pub fn needs(&self) -> Needs {
        let mut needs = Needs::default();

        for injection in self.active() {
            for value in injection.properties.values() {
                if let Some(name) = crate::module_reference(value) {
                    needs.modules.insert(name.to_string());
                } else if !value.starts_with('$') || value.starts_with("$require:") {
                    needs.asset_map = true;
                }
            }

            // In `keys`, everything with a `$` prefix is a literal.
            if injection.keys.values().any(|v| !v.starts_with('$')) {
                needs.asset_map = true;
            }
        }

        needs
    }
}

/// The inputs a config cannot be applied without.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Needs {
    /// A rule looks a key up in the asset map.
    pub asset_map: bool,
    /// Names of the module sources rules ask for, e.g. `assets`, `ids`.
    pub modules: BTreeSet<String>,
}
