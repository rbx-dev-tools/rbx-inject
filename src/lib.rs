//! Apply injection rules to a Roblox place file.
//!
//! One job: take a `.rbxl`, an asphalt asset map, and a set of rules, and write
//! the resolved ids into the place before it is uploaded. No network, no
//! credentials, no Roblox API. That is what makes it testable, and it is why it
//! is a separate binary from the tools that do talk to Roblox.

pub mod assets;
pub mod config;
pub mod dom;
pub mod luau;

use crate::assets::Assets;
use crate::config::Config;
use crate::luau::KeyValue;
use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{ustr, WeakDom};

/// What a run did, and what it could not do.
#[derive(Debug, Default)]
pub struct Report {
    /// One line per applied change, in config order.
    pub changes: Vec<String>,
    /// Rules that resolved to nothing. Not fatal by default: a place file often
    /// lags the config while a feature is being built.
    pub warnings: Vec<String>,
}

impl Report {
    pub fn changed(&self) -> bool {
        !self.changes.is_empty()
    }

    fn change(&mut self, line: impl Into<String>) {
        self.changes.push(line.into());
    }

    fn warn(&mut self, line: impl Into<String>) {
        self.warnings.push(line.into());
    }
}

/// Apply every rule to `dom`.
///
/// Properties are applied for every rule before any `keys` rule runs. The two
/// passes matter: a rule can replace a ModuleScript's whole `Source` with the
/// generated asset module, and a `keys` rule elsewhere may then edit a value
/// inside that new source. One pass would make the result depend on the order
/// rules happen to appear in the file.
pub fn apply(dom: &mut WeakDom, config: &Config, assets: &Assets) -> Report {
    let mut report = Report::default();

    for injection in config.active() {
        apply_properties(dom, injection, assets, &mut report);
    }

    for injection in config.active() {
        apply_keys(dom, injection, assets, &mut report);
    }

    report
}

fn apply_properties(
    dom: &mut WeakDom,
    injection: &config::Injection,
    assets: &Assets,
    report: &mut Report,
) {
    if injection.properties.is_empty() {
        return;
    }

    let path = &injection.roblox_path;

    // A rule that writes a module's source can create the module: the whole
    // point is that the file is generated, so requiring it to already exist in
    // the place would mean checking a generated stub into the .rbxl. Rules that
    // only set ordinary properties target something that must already be there.
    let writes_module_source = injection.properties.values().any(|v| {
        v == "$module" || v == "$rbxsync_module" || v.starts_with("$require:")
    });

    let target = if writes_module_source {
        match dom::ensure(dom, path, "ModuleScript") {
            Ok((r, created)) => {
                if created {
                    report.change(format!("{path}  [created ModuleScript]"));
                }
                r
            }
            Err(e) => {
                report.warn(format!("{path}: {e}"));
                return;
            }
        }
    } else {
        match dom::find(dom, path) {
            Some(r) => r,
            None => {
                report.warn(format!("{path}: no such instance"));
                return;
            }
        }
    };

    for (prop, value_key) in &injection.properties {
        let resolved = match resolve_property(value_key, assets) {
            Ok(v) => v,
            Err(e) => {
                report.warn(format!("{path}.{prop}: {e}"));
                continue;
            }
        };

        let Some(inst) = dom.get_by_ref_mut(target) else {
            report.warn(format!("{path}.{prop}: dangling reference"));
            continue;
        };

        match resolved {
            Resolved::Source(source) => {
                report.change(format!(
                    "{path}.{prop} = {} ({} bytes)",
                    describe_source(value_key),
                    source.len()
                ));
                inst.properties.insert(ustr(prop), Variant::String(source));
            }
            Resolved::Value(value) => {
                let variant = dom::variant_for(inst, prop, &value);
                report.change(format!("{path}.{prop} = {value}"));
                inst.properties.insert(ustr(prop), variant);
            }
        }
    }
}

fn apply_keys(
    dom: &mut WeakDom,
    injection: &config::Injection,
    assets: &Assets,
    report: &mut Report,
) {
    if injection.keys.is_empty() {
        return;
    }

    let path = &injection.roblox_path;

    let Some(target) = dom::find(dom, path) else {
        report.warn(format!("{path}: no such instance"));
        return;
    };

    let Some(inst) = dom.get_by_ref(target) else {
        report.warn(format!("{path}: dangling reference"));
        return;
    };

    if inst.class.as_str() != "ModuleScript" {
        report.warn(format!(
            "{path}: 'keys' needs a ModuleScript, found a {}",
            inst.class
        ));
        return;
    }

    let Some(Variant::String(source)) = inst.properties.get(&ustr("Source")) else {
        report.warn(format!("{path}: ModuleScript has no Source"));
        return;
    };
    let source = source.clone();

    let mut edits = Vec::with_capacity(injection.keys.len());
    for (key_path, value_key) in &injection.keys {
        match resolve_key(value_key, assets) {
            Ok(value) => {
                report.change(format!("{path}[{key_path}] = {}", describe(&value)));
                edits.push((key_path.clone(), value));
            }
            Err(e) => report.warn(format!("{path}[{key_path}]: {e}")),
        }
    }

    if edits.is_empty() {
        return;
    }

    match luau::apply_keys(&source, &edits) {
        Ok(new_source) => {
            if let Some(inst) = dom.get_by_ref_mut(target) {
                inst.properties
                    .insert(ustr("Source"), Variant::String(new_source));
            }
        }
        Err(e) => {
            // The edits were already reported as changes; take them back, since
            // nothing was written.
            report
                .changes
                .truncate(report.changes.len() - edits.len());
            report.warn(format!("{path}: {e:#}"));
        }
    }
}

enum Resolved {
    /// A whole module source, written verbatim.
    Source(String),
    /// A scalar, typed against the reflection database at the write site.
    Value(String),
}

/// Resolve the right-hand side of a `properties` entry.
fn resolve_property(value_key: &str, assets: &Assets) -> Result<Resolved, String> {
    if value_key == "$module" {
        return assets
            .module_source()
            .map(|s| Resolved::Source(s.to_string()))
            .ok_or_else(|| "$module needs --assets".to_string());
    }

    if value_key == "$rbxsync_module" {
        return assets
            .ids_module_source()
            .map(|s| Resolved::Source(s.to_string()))
            .ok_or_else(|| "$rbxsync_module needs --ids-module".to_string());
    }

    if let Some(key) = value_key.strip_prefix("$require:") {
        let resolved = assets
            .get(key)
            .ok_or_else(|| format!("'{key}' is not in the asset map"))?;

        let id = resolved
            .trim_start_matches("rbxassetid://")
            .trim_start_matches("rbxasset://");

        if id.parse::<u64>().is_err() {
            return Err(format!("'{key}' is '{resolved}', not a numeric asset id"));
        }

        return Ok(Resolved::Source(format!("return require({id})\n")));
    }

    assets
        .get(value_key)
        .map(|v| Resolved::Value(v.to_string()))
        .ok_or_else(|| format!("'{value_key}' is not in the asset map"))
}

/// Resolve the right-hand side of a `keys` entry.
///
/// A bare value is always an asset-map lookup, so that the common case stays
/// short. `$$` forces a literal string, which is the escape hatch for a value
/// that happens to look like a key.
fn resolve_key(value_key: &str, assets: &Assets) -> Result<KeyValue, String> {
    if let Some(literal) = value_key.strip_prefix("$$") {
        return Ok(KeyValue::Str(literal.to_string()));
    }

    if let Some(raw) = value_key.strip_prefix('$') {
        return Ok(match raw {
            "true" => KeyValue::Bool(true),
            "false" => KeyValue::Bool(false),
            "nil" => KeyValue::Nil,
            _ => match raw.parse::<f64>() {
                Ok(n) => KeyValue::Num(n),
                Err(_) => KeyValue::Str(raw.to_string()),
            },
        });
    }

    assets
        .get(value_key)
        .map(|v| KeyValue::Str(v.to_string()))
        .ok_or_else(|| format!("'{value_key}' is not in the asset map"))
}

fn describe(value: &KeyValue) -> String {
    match value {
        KeyValue::Str(s) => format!("{s:?}"),
        KeyValue::Num(n) => n.to_string(),
        KeyValue::Bool(b) => b.to_string(),
        KeyValue::Nil => "nil (removed)".to_string(),
    }
}

fn describe_source(value_key: &str) -> &str {
    match value_key {
        "$module" => "[asset module]",
        "$rbxsync_module" => "[ids module]",
        _ => "[require stub]",
    }
}
