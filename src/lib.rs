//! Apply injection rules to a Roblox place file.
//!
//! One job: take a `.rbxl`, an asphalt asset map, and a set of rules, and write
//! the resolved ids into the place before it is uploaded. No network, no
//! credentials, no Roblox API. That is what makes it testable, and it is why it
//! is a separate binary from the tools that do talk to Roblox.

pub mod check;
pub mod config;
pub mod dom;
pub mod inputs;
pub mod luau;

use crate::inputs::Inputs;
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
pub fn apply(dom: &mut WeakDom, config: &Config, inputs: &Inputs) -> Report {
    let mut report = Report::default();

    for injection in config.active() {
        apply_properties(dom, injection, inputs, &mut report);
    }

    for injection in config.active() {
        apply_keys(dom, injection, inputs, &mut report);
    }

    report
}

fn apply_properties(
    dom: &mut WeakDom,
    injection: &config::Injection,
    inputs: &Inputs,
    report: &mut Report,
) {
    if injection.properties.is_empty() {
        return;
    }

    let path = &injection.roblox_path;

    // Resolve every value before touching the place. Creating the target first
    // and discovering afterwards that nothing resolves leaves a new, empty
    // ModuleScript behind: a change the caller did not ask for, counted as a
    // change, written to disk, and exiting zero.
    let resolved: Vec<(&String, &String, Result<Resolved, String>)> = injection
        .properties
        .iter()
        .map(|(prop, value_key)| (prop, value_key, resolve_property(value_key, inputs)))
        .collect();

    for (prop, _, outcome) in &resolved {
        if let Err(e) = outcome {
            report.warn(format!("{path}.{prop}: {e}"));
        }
    }

    if resolved.iter().all(|(_, _, outcome)| outcome.is_err()) {
        return;
    }

    // A rule that writes a module's source can create the module: the whole
    // point is that the file is generated, so requiring it to already exist in
    // the place would mean checking a generated stub into the .rbxl. Rules that
    // only set ordinary properties target something that must already be there.
    let writes_module_source = resolved
        .iter()
        .any(|(_, _, outcome)| matches!(outcome, Ok(Resolved::Source(_))));

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

    for (prop, value_key, outcome) in resolved {
        let Ok(resolved) = outcome else { continue };

        let Some(inst) = dom.get_by_ref_mut(target) else {
            report.warn(format!("{path}.{prop}: dangling reference"));
            continue;
        };

        // The target may exist already and be the wrong kind of thing: a rule
        // writing a module's Source into a path that turns out to be a Folder
        // would be written, dropped by Roblox on load, and reported nowhere.
        //
        // Only refuse when the database knows the class and says the property is
        // not on it. An unknown class is a Roblox release the database has not
        // caught up with, and refusing there would break a working setup.
        let class = inst.class;
        if dom::class_is_known(class.as_str()) && dom::declared_type(class.as_str(), prop).is_none()
        {
            report.warn(format!(
                "{path}.{prop}: a {class} has no property '{prop}', skipped"
            ));
            continue;
        }

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
    inputs: &Inputs,
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
        match resolve_key(value_key, inputs) {
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

/// The module name a `properties` value asks for, if it asks for one.
///
/// `$module` and `$rbxsync_module` are the two names this tool used to hardcode,
/// kept as aliases so existing configs keep working. They are not special: a
/// generated ids module is the same kind of object as a generated asset module,
/// and `$module:<name>` says so.
pub fn module_reference(value_key: &str) -> Option<&str> {
    match value_key {
        "$module" => Some(inputs::ASSETS),
        "$rbxsync_module" => Some(inputs::IDS),
        v => v.strip_prefix("$module:"),
    }
}

/// Resolve the right-hand side of a `properties` entry.
fn resolve_property(value_key: &str, inputs: &Inputs) -> Result<Resolved, String> {
    if let Some(name) = module_reference(value_key) {
        return inputs
            .module(name)
            .map(|s| Resolved::Source(s.to_string()))
            .ok_or_else(|| format!("no module named '{name}' was given"));
    }

    if let Some(key) = value_key.strip_prefix("$require:") {
        let resolved = inputs
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

    inputs
        .get(value_key)
        .map(|v| Resolved::Value(v.to_string()))
        .ok_or_else(|| format!("'{value_key}' is not in the asset map"))
}

/// Resolve the right-hand side of a `keys` entry.
///
/// A bare value is always an asset-map lookup, so that the common case stays
/// short. `$$` forces a literal string, which is the escape hatch for a value
/// that happens to look like a key.
fn resolve_key(value_key: &str, inputs: &Inputs) -> Result<KeyValue, String> {
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

    inputs
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

fn describe_source(value_key: &str) -> String {
    match module_reference(value_key) {
        Some(name) => format!("[module '{name}']"),
        None => "[require stub]".to_string(),
    }
}
