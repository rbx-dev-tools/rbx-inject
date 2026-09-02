//! Injecting into a place that has already been injected into.
//!
//! This is the second deploy of the same game, which is every deploy after the
//! first, and it is not the same situation as the first one. Roblox migrates
//! legacy properties to newer ones and rbx-dom applies those migrations when it
//! *reads* a file, so the place that comes back is not shaped like the place
//! that went out: what was written as `Image` returns as `ImageContent`.
//!
//! The migration has a precedence rule, and it runs against us. When the new
//! property is already present, the old one is ignored. So writing a fresh
//! `Image` into a place that already carries `ImageContent` used to change
//! nothing at all: the tool reported the change, the file kept last deploy's id,
//! and nothing anywhere disagreed.

use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{ustr, InstanceBuilder, WeakDom};
use rbx_inject::{apply, config::Config, inputs::Inputs};

fn place() -> WeakDom {
    WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("StarterGui")
                .with_child(InstanceBuilder::new("ImageLabel").with_name("Icon"))
                .with_child(InstanceBuilder::new("Sound").with_name("Bang")),
        ),
    )
}

fn config(json: &str) -> Config {
    serde_json::from_str(json).expect("test config should parse")
}

/// Serialize and read back: what uploading and re-downloading a place does, and
/// what `apply` sees on the next run.
fn round_trip(dom: &WeakDom) -> WeakDom {
    let children = dom.get_by_ref(dom.root_ref()).unwrap().children().to_vec();
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, dom, &children).expect("place should serialize");
    rbx_binary::from_reader(bytes.as_slice()).expect("place should read back")
}

fn uri(dom: &WeakDom, path: &str, prop: &str) -> String {
    let r = rbx_inject::dom::find(dom, path).unwrap_or_else(|| panic!("no instance at {path}"));
    match dom.get_by_ref(r).unwrap().properties.get(&ustr(prop)) {
        Some(Variant::Content(c)) => c.as_uri().unwrap_or_default().to_string(),
        Some(Variant::ContentId(c)) => c.as_str().to_string(),
        other => panic!("{path}.{prop} is {other:?}"),
    }
}

/// Deploy, then deploy again with a new id, and read what the game would load.
#[test]
fn a_redeploy_with_a_new_id_actually_changes_the_id() {
    let rules = config(
        r#"{"injections":[{"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Icon"}}]}"#,
    );

    let mut dom = place();
    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://111")]),
    );

    // Upload, and come back to it next week.
    let mut dom = round_trip(&dom);
    assert_eq!(
        uri(&dom, "StarterGui.Icon", "ImageContent"),
        "rbxassetid://111"
    );

    let report = apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://222")]),
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let uploaded = round_trip(&dom);
    assert_eq!(
        uri(&uploaded, "StarterGui.Icon", "ImageContent"),
        "rbxassetid://222",
        "the redeploy kept the old id"
    );
}

/// Same for audio, whose migration lands on a differently named property.
#[test]
fn a_redeploy_replaces_a_sound_id_too() {
    let rules = config(
        r#"{"injections":[{"robloxPath":"StarterGui.Bang","properties":{"SoundId":"audio.Bang"}}]}"#,
    );

    let mut dom = place();
    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("audio.Bang", "rbxassetid://111")]),
    );
    let mut dom = round_trip(&dom);

    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("audio.Bang", "rbxassetid://222")]),
    );

    let uploaded = round_trip(&dom);
    assert_eq!(
        uri(&uploaded, "StarterGui.Bang", "AudioContent"),
        "rbxassetid://222"
    );
}

/// The stale twin is removed, not merely overwritten alongside. Leaving both in
/// the DOM would make the result depend on rbx-dom's precedence rather than on
/// what the config asked for.
#[test]
fn only_one_of_the_two_property_names_survives() {
    let rules = config(
        r#"{"injections":[{"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Icon"}}]}"#,
    );

    let mut dom = place();
    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://111")]),
    );
    let mut dom = round_trip(&dom);
    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://222")]),
    );

    let r = rbx_inject::dom::find(&dom, "StarterGui.Icon").unwrap();
    let props = &dom.get_by_ref(r).unwrap().properties;
    assert!(props.get(&ustr("Image")).is_some());
    assert!(
        props.get(&ustr("ImageContent")).is_none(),
        "the migrated twin should have been removed"
    );
}

/// And the report says so, because a property disappearing from a place file is
/// not something to do silently.
#[test]
fn the_report_names_the_property_it_replaced() {
    let rules = config(
        r#"{"injections":[{"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Icon"}}]}"#,
    );

    let mut dom = place();
    apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://111")]),
    );
    let mut dom = round_trip(&dom);
    let report = apply(
        &mut dom,
        &rules,
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://222")]),
    );

    assert!(
        report.changes[0].contains("replaced stale ImageContent"),
        "{:?}",
        report.changes
    );
}

/// A first deploy has nothing stale to clear, so it must not claim it did.
#[test]
fn a_first_deploy_reports_no_replacement() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[{"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Icon"}}]}"#,
        ),
        &Inputs::from_pairs([("ui.Icon", "rbxassetid://111")]),
    );

    assert_eq!(report.changes, ["StarterGui.Icon.Image = rbxassetid://111"]);
}

/// A property with no migration must not have anything removed around it.
#[test]
fn a_property_without_a_migration_is_left_alone() {
    let mut dom = WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("ReplicatedStorage").with_child(
                InstanceBuilder::new("ModuleScript")
                    .with_name("Config")
                    .with_property("Source", "return {}"),
            ),
        ),
    );

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.Config","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &Inputs::default().with_module_source("assets", "return { a = 1 }"),
    );

    assert_eq!(report.changes.len(), 1);
    assert!(!report.changes[0].contains("stale"), "{:?}", report.changes);
}

/// Running the whole cycle twice with unchanged inputs must converge, or every
/// deploy shows a diff of a file that did not really change.
#[test]
fn redeploying_the_same_id_is_stable() {
    let rules = config(
        r#"{"injections":[{"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Icon"}}]}"#,
    );
    let inputs = || Inputs::from_pairs([("ui.Icon", "rbxassetid://111")]);

    let mut dom = place();
    apply(&mut dom, &rules, &inputs());
    let mut dom = round_trip(&dom);
    apply(&mut dom, &rules, &inputs());
    let second = round_trip(&dom);

    assert_eq!(
        uri(&second, "StarterGui.Icon", "ImageContent"),
        "rbxassetid://111"
    );

    let r = rbx_inject::dom::find(&second, "StarterGui.Icon").unwrap();
    assert_eq!(
        second.get_by_ref(r).unwrap().properties.len(),
        1,
        "a stable redeploy should not accumulate properties"
    );
}
