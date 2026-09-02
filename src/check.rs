//! Validate rules against a place, without changing anything.
//!
//! Injection targets instances by path, and a path is a string nobody in Studio
//! knows they are breaking. Rename `ReplicatedStorage.Assets.Characters.Player`
//! and every rule underneath it quietly resolves to nothing: the deploy still
//! succeeds and the game ships with an empty AnimationId.
//!
//! `check` is that failure, moved earlier. It needs no asset map and no
//! credentials, so it runs in a pre-commit hook or in CI, the day the rename
//! happens rather than at the next deploy.

use crate::config::{Config, Injection};
use crate::dom;
use crate::inputs::Inputs;
use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{ustr, WeakDom};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The rule cannot apply. Something is wrong with the place or the config.
    Error,
    /// The rule will apply, but probably not as intended.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub target: String,
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.severity, self.target, self.message)
    }
}

/// Check every rule. `assets` is optional: without it, asset-map lookups are not
/// checked, which is the point of being able to run before anything is uploaded.
pub fn check(dom: &WeakDom, config: &Config, inputs: Option<&Inputs>) -> Vec<Finding> {
    let mut findings = Vec::new();

    for injection in config.active() {
        check_properties(dom, injection, inputs, &mut findings);
        check_keys(dom, injection, inputs, &mut findings);
    }

    findings
}

fn check_properties(
    dom: &WeakDom,
    injection: &Injection,
    inputs: Option<&Inputs>,
    findings: &mut Vec<Finding>,
) {
    if injection.properties.is_empty() {
        return;
    }

    let path = &injection.roblox_path;

    let creates = injection
        .properties
        .values()
        .any(|v| crate::module_reference(v).is_some() || v.starts_with("$require:"));

    let target = dom::find(dom, path);

    if target.is_none() {
        // A rule that writes a module source creates what it targets, so a
        // missing path is fine as long as the *service* it hangs off exists.
        if creates {
            let service = path.split('.').next().unwrap_or_default();
            if dom::find(dom, service).is_none() {
                findings.push(Finding {
                    severity: Severity::Error,
                    target: path.clone(),
                    message: format!("'{service}' is not a service of this place"),
                });
            }
        } else {
            findings.push(Finding {
                severity: Severity::Error,
                target: path.clone(),
                message: "no such instance".to_string(),
            });
        }
        return;
    }

    let class = dom
        .get_by_ref(target.expect("checked above"))
        .map(|i| i.class.to_string())
        .unwrap_or_default();

    for (prop, value_key) in &injection.properties {
        // A misspelled property name is not an error the serializer catches: it
        // writes the property, Roblox loads the place and drops it, and nothing
        // anywhere says so. Only a warning, because the database always lags the
        // newest Roblox release by a few weeks.
        if dom::declared_type(&class, prop).is_none() {
            findings.push(Finding {
                severity: Severity::Warning,
                target: format!("{path}.{prop}"),
                message: format!("{class} has no property '{prop}' in the reflection database"),
            });
        }

        check_value(inputs, value_key, &format!("{path}.{prop}"), findings);
    }
}

fn check_keys(
    dom: &WeakDom,
    injection: &Injection,
    inputs: Option<&Inputs>,
    findings: &mut Vec<Finding>,
) {
    if injection.keys.is_empty() {
        return;
    }

    let path = &injection.roblox_path;

    let Some(target) = dom::find(dom, path) else {
        findings.push(Finding {
            severity: Severity::Error,
            target: path.clone(),
            message: "no such instance".to_string(),
        });
        return;
    };

    let Some(inst) = dom.get_by_ref(target) else {
        return;
    };

    if inst.class.as_str() != "ModuleScript" {
        findings.push(Finding {
            severity: Severity::Error,
            target: path.clone(),
            message: format!("'keys' needs a ModuleScript, found a {}", inst.class),
        });
        return;
    }

    match inst.properties.get(&ustr("Source")) {
        Some(Variant::String(source)) => {
            // Evaluating it here is the only way to know the rewrite will work.
            // A module that returns a function, or does not parse, fails at
            // deploy time otherwise.
            if let Err(e) = crate::luau::apply_keys(source, &[]) {
                findings.push(Finding {
                    severity: Severity::Error,
                    target: path.clone(),
                    message: format!("{e:#}"),
                });
                return;
            }
        }
        _ => {
            findings.push(Finding {
                severity: Severity::Error,
                target: path.clone(),
                message: "ModuleScript has no Source".to_string(),
            });
            return;
        }
    }

    for (key_path, value_key) in &injection.keys {
        check_value(inputs, value_key, &format!("{path}[{key_path}]"), findings);
    }
}

/// Asset-map lookups, only when a map was supplied.
fn check_value(
    inputs: Option<&Inputs>,
    value_key: &str,
    target: &str,
    findings: &mut Vec<Finding>,
) {
    let Some(inputs) = inputs else { return };

    // A module reference names a file, not a key in the map.
    if crate::module_reference(value_key).is_some() {
        return;
    }

    // Everything else with a `$` prefix is a literal.
    let lookup = match value_key.strip_prefix("$require:") {
        Some(key) => key,
        None if value_key.starts_with('$') => return,
        None => value_key,
    };

    if inputs.get(lookup).is_none() {
        findings.push(Finding {
            severity: Severity::Error,
            target: target.to_string(),
            message: format!("'{lookup}' is not in the asset map"),
        });
    }
}
