//! Navigating and typing a Roblox DOM.

use rbx_dom_weak::types::{Content, ContentId, Ref, Variant, VariantType};
use rbx_dom_weak::{ustr, Instance, InstanceBuilder, WeakDom};
use rbx_reflection::{PropertyKind, PropertySerialization};

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

/// Whether the reflection database knows this class at all.
///
/// The distinction matters wherever a missing property decides something: an
/// unknown *property* on a known class is a mistake, while an unknown *class* is
/// a Roblox release the database has not caught up with.
pub fn class_is_known(class_name: &str) -> bool {
    rbx_reflection_database::get()
        .ok()
        .is_some_and(|db| db.classes.contains_key(class_name))
}

/// The type the reflection database declares for `class.prop`, walking up the
/// superclass chain. `None` when either is unknown to the database.
pub fn declared_type(class_name: &str, prop_name: &str) -> Option<VariantType> {
    descriptor(class_name, prop_name).map(|p| p.data_type.ty())
}

/// Property names that would override `prop_name` when the place is read back.
///
/// Roblox is migrating legacy properties to newer ones, and rbx-dom applies
/// those migrations on read with a documented precedence: when the new property
/// is already present, the old one is ignored.
///
/// That turns a redeploy into a silent no-op. A place written last week comes
/// back carrying `ImageContent`, because reading migrated the `Image` we wrote.
/// Writing a fresh `Image` into it leaves both, and on the next read the stale
/// `ImageContent` wins. The tool reports the change, the file contains the old
/// id, and nothing anywhere disagrees. So the stale twin has to go.
///
/// Removing it rather than converting the value ourselves is deliberate: it
/// leaves the file in a state Studio could have produced, and lets rbx-dom's own
/// reader run the migration, one-to-many cases included, instead of this crate
/// reimplementing a table that changes with every Roblox release.
pub fn overriding_properties(class_name: &str, prop_name: &str) -> Vec<String> {
    let Some(prop) = descriptor(class_name, prop_name) else {
        return Vec::new();
    };

    if let PropertyKind::Canonical {
        serialization: PropertySerialization::Migrate(migration),
    } = &prop.kind
    {
        return migration
            .new_property_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect();
    }

    Vec::new()
}

/// The descriptor for `class.prop`, walking up the superclass chain.
fn descriptor<'a>(
    class_name: &str,
    prop_name: &str,
) -> Option<&'a rbx_reflection::PropertyDescriptor<'a>> {
    let db = rbx_reflection_database::get().ok()?;
    let mut class = db.classes.get(class_name)?;

    loop {
        if let Some(prop) = class.properties.get(prop_name) {
            return Some(prop);
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
