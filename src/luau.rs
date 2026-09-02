//! Editing the table a ModuleScript returns.
//!
//! A `keys` rule cannot be done with string surgery on the source: the target is
//! a value inside a Luau table, addressed by a path, and the file has to still
//! be valid Luau afterwards. So the source is evaluated, the table is edited as
//! a table, and it is printed back out.
//!
//! Luau is embedded (`mlua`, vendored) rather than shelled out to. Shelling out
//! to Lune for this one step is what put a second Roblox-format parser in the
//! pipeline, on its own release cadence, and that second parser is what broke
//! when Roblox changed the Tags encoding.

use anyhow::{bail, Context, Result};
use mlua::{Lua, Table, Value};

/// A value an injection can write into a module table.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyValue {
    Str(String),
    Num(f64),
    Bool(bool),
    /// Removes the key.
    Nil,
}

/// Evaluate `source`, apply `edits`, and print the table back as a module.
///
/// Returns the new source. Keys are printed in sorted order, so re-running with
/// unchanged inputs produces a byte-identical file and a place diff stays
/// readable.
pub fn apply_keys(source: &str, edits: &[(String, KeyValue)]) -> Result<String> {
    let lua = Lua::new();

    let value: Value = lua
        .load(source)
        .set_name("module")
        .eval()
        .context("evaluating the ModuleScript source")?;

    let Value::Table(table) = value else {
        bail!("the ModuleScript does not return a table");
    };

    for (path, value) in edits {
        set_nested(&lua, &table, path, value)
            .with_context(|| format!("setting key '{path}'"))?;
    }

    Ok(format!("return {}\n", serialize(&table, "")?))
}

/// Walk a dotted key path, creating intermediate tables, and assign the leaf.
fn set_nested(lua: &Lua, root: &Table, path: &str, value: &KeyValue) -> Result<()> {
    let parts: Vec<&str> = path.split('.').collect();
    let (leaf, branches) = parts.split_last().expect("split always yields one part");

    let mut current = root.clone();
    for part in branches {
        let key = parse_key(lua, part)?;
        let next = match current.get::<Value>(key.clone())? {
            Value::Table(t) => t,
            // Anything that is not a table gets replaced. The alternative is
            // failing on a config that asks for `a.b` where `a` is a number,
            // and the user's intent there is unambiguous.
            _ => {
                let fresh = lua.create_table()?;
                current.set(key, &fresh)?;
                fresh
            }
        };
        current = next;
    }

    let key = parse_key(lua, leaf)?;
    match value {
        KeyValue::Str(s) => current.set(key, s.as_str())?,
        KeyValue::Num(n) => current.set(key, *n)?,
        KeyValue::Bool(b) => current.set(key, *b)?,
        KeyValue::Nil => current.set(key, Value::Nil)?,
    }

    Ok(())
}

/// `"$4"` addresses the integer key `4`, `"name"` the string key `"name"`.
///
/// Without the prefix there is no way to write `t[4]` from a config, because
/// every key in JSON or TOML is a string, and `t["4"]` is a different slot.
fn parse_key(lua: &Lua, key: &str) -> Result<Value> {
    if let Some(raw) = key.strip_prefix('$') {
        // Luau's integer type is 32-bit, so a key beyond that range is not a
        // table slot we could address anyway; it falls through to a string key.
        if let Ok(n) = raw.parse::<mlua::Integer>() {
            return Ok(Value::Integer(n));
        }
        return Ok(Value::String(lua.create_string(raw)?));
    }
    Ok(Value::String(lua.create_string(key)?))
}

/// Print a Luau value as source.
fn serialize(table: &Table, indent: &str) -> Result<String> {
    let inner_indent = format!("{indent}\t");
    let mut entries: Vec<(Value, Value)> = Vec::new();

    for pair in table.clone().pairs::<Value, Value>() {
        entries.push(pair?);
    }

    if entries.is_empty() {
        return Ok("{}".to_string());
    }

    let mut lines = Vec::with_capacity(entries.len());

    if let Some(len) = dense_array_len(&entries) {
        for i in 1..=len {
            let value = table.get::<Value>(i as mlua::Integer)?;
            lines.push(format!(
                "{inner_indent}{},",
                serialize_value(&value, &inner_indent)?
            ));
        }
    } else {
        // Sorted by printed form, which is what makes two runs comparable. Lua
        // iteration order is a hash order and would reshuffle the file on every
        // run for no reason.
        entries.sort_by_cached_key(|(k, _)| key_sort_form(k));

        for (key, value) in &entries {
            lines.push(format!(
                "{inner_indent}{} = {},",
                serialize_key(key, &inner_indent)?,
                serialize_value(value, &inner_indent)?
            ));
        }
    }

    Ok(format!("{{\n{}\n{indent}}}", lines.join("\n")))
}

/// `Some(n)` when the keys are exactly the integers `1..=n`.
fn dense_array_len(entries: &[(Value, Value)]) -> Option<usize> {
    let mut max = 0i64;

    for (key, _) in entries {
        let n = match key {
            Value::Integer(i) => i64::from(*i),
            Value::Number(f) if f.fract() == 0.0 => *f as i64,
            _ => return None,
        };
        if n < 1 {
            return None;
        }
        max = max.max(n);
    }

    // Dense means no gaps: `{[1]=a, [3]=b}` has a max of 3 and two entries, and
    // printing it as an array would move `b` from slot 3 to slot 2.
    (max == entries.len() as i64).then_some(max as usize)
}

fn key_sort_form(key: &Value) -> String {
    match key {
        Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => format_number(*n),
        other => format!("{other:?}"),
    }
}

/// A bare identifier where Luau allows one, `[expr]` otherwise.
fn serialize_key(key: &Value, indent: &str) -> Result<String> {
    if let Value::String(s) = key {
        let s = s.to_str()?;
        if is_identifier(&s) {
            return Ok(s.to_string());
        }
    }
    Ok(format!("[{}]", serialize_value(key, indent)?))
}

fn serialize_value(value: &Value, indent: &str) -> Result<String> {
    Ok(match value {
        Value::String(s) => quote(&s.to_str()?),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => format_number(*n),
        Value::Boolean(b) => b.to_string(),
        Value::Table(t) => serialize(t, indent)?,
        Value::Nil => "nil".to_string(),
        // Functions, userdata and threads have no source form. A module holding
        // one is a config table with logic in it, and rewriting it would silently
        // drop that logic, so refuse instead.
        other => bail!(
            "cannot write a {} back to source",
            other.type_name()
        ),
    })
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        format!("{}", n as i64)
    } else {
        // Rust's Display for f64 is shortest-roundtrip, which is what Luau's
        // tostring gives too.
        format!("{n}")
    }
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !is_keyword(s)
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "and" | "break" | "do" | "else" | "elseif" | "end" | "false" | "for" | "function"
            | "if" | "in" | "local" | "nil" | "not" | "or" | "repeat" | "return" | "then"
            | "true" | "until" | "while" | "continue" | "export" | "type"
    )
}

/// Quote a string the way Luau's `%q` does.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');

    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }

    out.push('"');
    out
}
