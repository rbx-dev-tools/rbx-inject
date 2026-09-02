//! What an injection can resolve against.
//!
//! Two different things, deliberately not one flag:
//!
//! - **the map**, which is *evaluated*: asphalt's output, read as data, so that
//!   `ui.ShopIcon` resolves to an id.
//! - **modules**, which are *copied verbatim*: any generated Luau file whose
//!   whole text goes into a ModuleScript's `Source`.
//!
//! Asphalt's output happens to be both, and that is the only reason it has a
//! flag of its own. Every other generated module (a shop ids module, an env
//! module, a build-info module) is the same kind of object and goes through the
//! same generic mechanism, rather than earning a new flag each time.

use anyhow::{bail, Context, Result};
use mlua::{Lua, Table, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// The conventional name for asphalt's output, so `$module` keeps meaning what
/// it always meant.
pub const ASSETS: &str = "assets";

/// The conventional name for the generated shop/ids module, so
/// `$rbxsync_module` keeps meaning what it always meant.
pub const IDS: &str = "ids";

#[derive(Debug, Default)]
pub struct Inputs {
    /// Flattened lookup map: `"audio.Bang" -> "rbxassetid://123"`.
    map: BTreeMap<String, String>,
    /// Named module sources, copied verbatim into a `Source`.
    modules: BTreeMap<String, String>,
}

impl Inputs {
    /// Read asphalt's `Assets.luau`: both the lookup map and the module named
    /// [`ASSETS`].
    ///
    /// The file is *evaluated*, not scanned line by line. The previous
    /// implementation matched `name = "rbxassetid://…"` with string prefixes,
    /// which quietly dropped anything nested more than two levels deep, anything
    /// written on one line, and anything with a comment after it. Asphalt emits
    /// Luau; the only parser guaranteed to agree with asphalt is Luau.
    pub fn with_asphalt(mut self, path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading asphalt output {}", path.display()))?;

        let lua = Lua::new();
        let value: Value = lua
            .load(&source)
            .set_name(path.to_string_lossy().as_ref())
            .eval()
            .with_context(|| format!("evaluating asphalt output {}", path.display()))?;

        let Value::Table(table) = value else {
            bail!("asphalt output {} did not return a table", path.display());
        };

        flatten(&table, "", &mut self.map)?;
        self.modules.insert(ASSETS.to_string(), source);

        Ok(self)
    }

    /// Register any file as an injectable module source, under `name`.
    pub fn with_module(mut self, name: &str, path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading module '{name}' from {}", path.display()))?;
        self.modules.insert(name.to_string(), source);
        Ok(self)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn module(&self, name: &str) -> Option<&str> {
        self.modules.get(name).map(String::as_str)
    }

    pub fn has_map(&self) -> bool {
        !self.map.is_empty()
    }

    /// Every registered module name, for error messages that say what is there.
    pub fn module_names(&self) -> impl Iterator<Item = &str> {
        self.modules.keys().map(String::as_str)
    }

    /// Build a map directly, for callers that already have one and for tests.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            map: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            modules: BTreeMap::new(),
        }
    }

    /// Register a module source without reading a file.
    pub fn with_module_source(mut self, name: &str, source: impl Into<String>) -> Self {
        self.modules.insert(name.to_string(), source.into());
        self
    }
}

/// Walk nested tables into dotted keys. Depth is not capped at the two levels
/// asphalt happens to emit today, because `ui.icons.shop.Cart` is a shape a user
/// can already write and there is no reason for it to resolve to nothing.
fn flatten(table: &Table, prefix: &str, out: &mut BTreeMap<String, String>) -> Result<()> {
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;

        let key = match key {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            // A float or table key in an asset map is not something we can name
            // in a dotted lookup, so it is skipped rather than mangled.
            _ => continue,
        };

        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };

        match value {
            Value::String(s) => {
                out.insert(path, s.to_str()?.to_string());
            }
            Value::Table(t) => flatten(&t, &path, out)?,
            _ => {}
        }
    }

    Ok(())
}
