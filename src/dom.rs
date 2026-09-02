//! Navigating and typing a Roblox DOM.

use rbx_dom_weak::types::{Content, ContentId, Ref, Variant, VariantType};
use rbx_dom_weak::{ustr, Instance, InstanceBuilder, WeakDom};

/// Resolve a dot-separated path from the DataModel down.
pub fn find(dom: &WeakDom, path: &str) -> Option<Ref> {
    let mut current = dom.root_ref();

    for part in path.split('.') {
        let children = dom.get_by_ref(current)?.children().to_vec();
        current = *children
            .iter()
            .find(|&&r| dom.get_by_ref(r).is_some_and(|i| i.name == part))?;
    }

    Some(current)
}

/// Resolve a path, creating what is missing: `Folder` for the intermediate
/// segments, `leaf_class` for the last one. Returns the leaf and whether
/// anything was created.
///
/// The first segment is never created. It is a service, services always exist in
/// a real place file, and a typo like `ReplicatedStorge` would otherwise
/// silently grow a Folder next to the real service and inject into it, which
/// looks like success and ships a broken place.
pub fn ensure(dom: &mut WeakDom, path: &str, leaf_class: &str) -> Result<(Ref, bool), String> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("path '{path}' has an empty segment"));
    }

    let mut current = dom.root_ref();
    let mut created = false;

    for (i, part) in parts.iter().enumerate() {
        let children = dom
            .get_by_ref(current)
            .ok_or("dangling reference while walking path")?
            .children()
            .to_vec();

        let existing = children
            .iter()
            .copied()
            .find(|&r| dom.get_by_ref(r).is_some_and(|inst| inst.name == *part));

        current = match existing {
            Some(found) => found,
            None if i == 0 => return Err(format!("'{part}' is not a service of this place")),
            None => {
                let class = if i == parts.len() - 1 {
                    leaf_class
                } else {
                    "Folder"
                };
                created = true;
                dom.insert(current, InstanceBuilder::new(class).with_name(*part))
            }
        };
    }

    Ok((current, created))
}

/// Build the `Variant` to write into `instance.prop_name` for a string value.
///
/// The reflection database decides, not the property name. Roblox split the
/// legacy string-shaped `ContentId` from the modern `Content` userdata, and the
/// two are not interchangeable: `ImageLabel.Image` is a ContentId while
/// `ImageLabel.ImageContent` is a Content, and writing one where the other
/// belongs makes rbx_binary refuse to serialize the file at all. No list of
/// property names stays right across Roblox releases; the database ships with
/// them.
pub fn variant_for(inst: &Instance, prop_name: &str, value: &str) -> Variant {
    if let Some(v) = declared_type(inst.class.as_str(), prop_name).and_then(|t| build(t, value)) {
        return v;
    }

    // The database does not know this class or property. It might be brand new,
    // or from a Roblox build the database predates, so fall back to whatever
    // type the place file already has there.
    if let Some(v) = inst
        .properties
        .get(&ustr(prop_name))
        .and_then(|existing| build(existing.ty(), value))
    {
        return v;
    }

    if value.starts_with("rbxassetid://") || value.starts_with("rbxasset://") {
        Variant::ContentId(ContentId::from(value))
    } else {
        Variant::String(value.to_string())
    }
}

/// The type the reflection database declares for `class.prop`, walking up the
/// superclass chain.
fn declared_type(class_name: &str, prop_name: &str) -> Option<VariantType> {
    let db = rbx_reflection_database::get().ok()?;
    let mut class = db.classes.get(class_name)?;

    loop {
        if let Some(prop) = class.properties.get(prop_name) {
            return Some(prop.data_type.ty());
        }
        class = db.classes.get(class.superclass?)?;
    }
}

/// A `Variant` of the given type from a string, or `None` for types an injection
/// cannot express. `None` sends the caller to its next fallback rather than
/// writing a value the serializer would reject.
fn build(ty: VariantType, value: &str) -> Option<Variant> {
    Some(match ty {
        VariantType::Content => Variant::Content(Content::from_uri(value)),
        VariantType::ContentId => Variant::ContentId(ContentId::from(value)),
        VariantType::String => Variant::String(value.to_string()),
        VariantType::Bool => Variant::Bool(value == "true"),
        VariantType::Int32 => Variant::Int32(value.parse().ok()?),
        VariantType::Int64 => Variant::Int64(value.parse().ok()?),
        VariantType::Float32 => Variant::Float32(value.parse().ok()?),
        VariantType::Float64 => Variant::Float64(value.parse().ok()?),
        _ => return None,
    })
}
