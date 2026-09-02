//! The asset map: what `ui.ShopIcon` resolves to.

use anyhow::{bail, Context, Result};
use mlua::{Lua, Table, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Everything an injection can look a value up in.
#[derive(Debug, Default)]
pub struct Assets {
    /// Flattened asphalt output: `"audio.Bang" -> "rbxassetid://123"`.
    map: BTreeMap<String, String>,
    /// Verbatim source of the asphalt module, for `$module`.
    module_source: Option<String>,
    /// Verbatim source of the generated shop/ids module, for `$rbxsync_module`.
    ids_module_source: Option<String>,
}

impl Assets {
    /// Read an asphalt-generated `Assets.luau`.
    ///
    /// The file is *evaluated*, not scanned line by line. The previous
    /// implementation matched `name = "rbxassetid://…"` with string prefixes,
    /// which quietly dropped anything nested more than two levels deep, anything
    /// written on one line, and anything with a comment after it. Asphalt emits
    /// Luau; the only parser guaranteed to agree with asphalt is Luau.
    pub fn from_asphalt(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading asphalt output {}", path.display()))?;

        let lua = Lua::new();
        let value: Value = lua
            .load(&source)
            .set_name(path.to_string_lossy().as_ref())
            .eval()
            .with_context(|| format!("evaluating asphalt output {}", path.display()))?;

        let Value::Table(table) = value else {
            bail!(
                "asphalt output {} did not return a table",
                path.display()
            );
        };

        let mut map = BTreeMap::new();
        flatten(&table, "", &mut map)?;

        Ok(Self {
            map,
            module_source: Some(source),
            ids_module_source: None,
        })
    }

    /// Register the generated ids module (`rbx shop codegen`, or rbxsync's
    /// output) so `$rbxsync_module` can resolve to its source.
    pub fn with_ids_module(mut self, path: &Path) -> Result<Self> {
        self.ids_module_source = Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading ids module {}", path.display()))?,
        );
        Ok(self)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn module_source(&self) -> Option<&str> {
        self.module_source.as_deref()
    }

    pub fn ids_module_source(&self) -> Option<&str> {
        self.ids_module_source.as_deref()
    }

    /// Every key, for error messages that suggest what the user meant.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
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
            module_source: None,
            ids_module_source: None,
        }
    }

    /// Set the `$module` source without reading a file.
    pub fn with_module_source(mut self, source: impl Into<String>) -> Self {
        self.module_source = Some(source.into());
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
