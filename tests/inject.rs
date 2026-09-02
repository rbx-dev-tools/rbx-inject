//! End-to-end tests over a synthetic place.
//!
//! The place is built in memory rather than committed as a fixture. A .rbxl in
//! the repository would be an opaque blob nobody can review in a diff, and one
//! saved from a real project carries that project's ids inside it.

use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{ustr, InstanceBuilder, WeakDom};
use rbx_inject::{apply, config::Config, inputs::Inputs, Report};

/// A place with the shapes the rules target: a config module, an image, a sound.
fn place() -> WeakDom {
    WeakDom::new(
        InstanceBuilder::new("DataModel")
            .with_child(
                InstanceBuilder::new("ReplicatedStorage").with_child(
                    InstanceBuilder::new("ModuleScript")
                        .with_name("GameConfig")
                        .with_property(
                            "Source",
                            "return { Sounds = { Bang = \"\" }, Settings = { Volume = 1 } }",
                        ),
                ),
            )
            .with_child(
                InstanceBuilder::new("StarterGui")
                    .with_child(InstanceBuilder::new("ImageLabel").with_name("Icon"))
                    .with_child(InstanceBuilder::new("Sound").with_name("Bang")),
            ),
    )
}

fn inputs() -> Inputs {
    Inputs::from_pairs([
        ("ui.ShopIcon", "rbxassetid://111"),
        ("audio.Bang", "rbxassetid://222"),
        ("models.main", "rbxassetid://333"),
        ("deep.nested.value", "rbxassetid://444"),
    ])
}

fn config(json: &str) -> Config {
    serde_json::from_str(json).expect("test config should parse")
}

fn prop(dom: &WeakDom, path: &str, name: &str) -> Variant {
    let r = rbx_inject::dom::find(dom, path).unwrap_or_else(|| panic!("no instance at {path}"));
    dom.get_by_ref(r)
        .unwrap()
        .properties
        .get(&ustr(name))
        .unwrap_or_else(|| panic!("{path} has no {name}"))
        .clone()
}

fn source(dom: &WeakDom, path: &str) -> String {
    match prop(dom, path, "Source") {
        Variant::String(s) => s,
        other => panic!("Source is {other:?}"),
    }
}

/// The asset uri on an instance, under whichever content property holds it.
///
/// Roblox is migrating the legacy string-shaped properties to the modern
/// `Content` type, and rbx_binary applies those migrations *on read*: a place
/// written with `Image` comes back with `ImageContent`. So a test that pins the
/// property name across a round trip is testing rbx-dom's migration table, not
/// this crate. What matters is that the id arrives and survives.
fn content_uri(dom: &WeakDom, path: &str, names: &[&str]) -> String {
    let r = rbx_inject::dom::find(dom, path).unwrap_or_else(|| panic!("no instance at {path}"));
    let inst = dom.get_by_ref(r).unwrap();

    for name in names {
        match inst.properties.get(&ustr(name)) {
            Some(Variant::ContentId(id)) => return id.as_str().to_string(),
            Some(Variant::Content(c)) => {
                return c.as_uri().unwrap_or_default().to_string();
            }
            _ => {}
        }
    }

    panic!(
        "{path} has none of {names:?}; it has {:?}",
        inst.properties.keys().collect::<Vec<_>>()
    )
}

/// Serialize and read back, the way the pipeline does before uploading.
fn round_trip(dom: &WeakDom) -> WeakDom {
    let children = dom.get_by_ref(dom.root_ref()).unwrap().children().to_vec();
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, dom, &children).expect("place should serialize");
    rbx_binary::from_reader(bytes.as_slice()).expect("place should read back")
}

// ─── Properties ──────────────────────────────────────────────

/// The regression that motivated this crate. `ImageLabel.Image` is a
/// `ContentId`, not a `Content`; the two are distinct types with no naming rule
/// separating them, and writing the wrong one makes the *serializer* fail, far
/// from the line that chose it.
#[test]
fn image_gets_the_type_the_database_declares() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.ShopIcon"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    match prop(&dom, "StarterGui.Icon", "Image") {
        Variant::ContentId(id) => assert_eq!(id.as_str(), "rbxassetid://111"),
        other => panic!("Image should be a ContentId, got {other:?}"),
    }

    // And the whole point: the place still serializes, and the id survives.
    // A `Content` written where a `ContentId` belongs makes `to_writer` fail
    // here rather than at upload time.
    let reread = round_trip(&dom);
    assert_eq!(
        content_uri(&reread, "StarterGui.Icon", &["Image", "ImageContent"]),
        "rbxassetid://111"
    );
}

/// Documents the migration itself, because it surprises everyone once: what you
/// write is not the property name you read back.
#[test]
fn reading_back_applies_robloxs_property_migration() {
    let mut dom = place();
    apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.ShopIcon"}}
            ]}"#,
        ),
        &inputs(),
    );

    let reread = round_trip(&dom);
    let r = rbx_inject::dom::find(&reread, "StarterGui.Icon").unwrap();
    let props = &reread.get_by_ref(r).unwrap().properties;

    assert!(props.get(&ustr("Image")).is_none(), "Image should be gone");
    match props.get(&ustr("ImageContent")) {
        Some(Variant::Content(c)) => assert_eq!(c.as_uri(), Some("rbxassetid://111")),
        other => panic!("expected ImageContent, got {other:?}"),
    }
}

#[test]
fn sound_id_is_written_and_survives_a_round_trip() {
    let mut dom = place();
    apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Bang","properties":{"SoundId":"audio.Bang"}}
            ]}"#,
        ),
        &inputs(),
    );

    let reread = round_trip(&dom);
    assert_eq!(
        // `SoundId` migrates to `AudioContent`, not to the `SoundContent` the
        // naming pattern would suggest. Another reason no hardcoded list of
        // property names stays correct.
        content_uri(&reread, "StarterGui.Bang", &["SoundId", "AudioContent"]),
        "rbxassetid://222"
    );
}

#[test]
fn module_source_creates_the_missing_module_script() {
    let mut dom = place();
    let inputs = inputs().with_module_source("assets", "return { ui = { ShopIcon = \"rbxassetid://111\" } }");

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.Generated.Assets","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &inputs,
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(source(&dom, "ReplicatedStorage.Generated.Assets").contains("ShopIcon"));

    // The intermediate segment becomes a Folder, the leaf a ModuleScript.
    let folder = rbx_inject::dom::find(&dom, "ReplicatedStorage.Generated").unwrap();
    assert_eq!(dom.get_by_ref(folder).unwrap().class.as_str(), "Folder");
}

/// The other half of the same rule: when the module is already in the place,
/// its Source is replaced rather than a second one being created beside it.
#[test]
fn module_source_overwrites_a_module_that_already_exists() {
    let mut dom = place();
    let inputs = inputs().with_module_source("assets", "return { fresh = true }");

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.GameConfig","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &inputs,
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        source(&dom, "ReplicatedStorage.GameConfig"),
        "return { fresh = true }"
    );

    // One module, not two: nothing was created beside the existing one.
    let parent = rbx_inject::dom::find(&dom, "ReplicatedStorage").unwrap();
    let named: Vec<_> = dom
        .get_by_ref(parent)
        .unwrap()
        .children()
        .iter()
        .filter(|r| dom.get_by_ref(**r).is_some_and(|i| i.name == "GameConfig"))
        .collect();
    assert_eq!(named.len(), 1);
}

/// The third case, and the one that used to be silent: the path exists, but as
/// the wrong kind of thing. A Folder has no Source, so Roblox would drop the
/// property on load and nothing would say so.
#[test]
fn a_module_rule_aimed_at_a_folder_is_refused() {
    let mut dom = WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("ReplicatedStorage")
                .with_child(InstanceBuilder::new("Folder").with_name("GameIds")),
        ),
    );

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.GameIds","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &inputs().with_module_source("assets", "return {}"),
    );

    assert!(!report.changed(), "{:?}", report.changes);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("Folder has no property 'Source'"),
        "{:?}",
        report.warnings
    );
}

/// A missing input must not leave a new empty ModuleScript behind. Before this,
/// forgetting `--assets` created the target, failed to fill it, counted that as
/// a change, and wrote the file.
#[test]
fn a_rule_whose_module_is_missing_creates_nothing() {
    let mut dom = place();

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.GameIds","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert!(!report.changed(), "{:?}", report.changes);
    assert!(rbx_inject::dom::find(&dom, "ReplicatedStorage.GameIds").is_none());
}

/// A generated ids module is the same kind of object as a generated asset
/// module, so it goes through the same generic mechanism under any name.
#[test]
fn any_named_module_can_be_injected() {
    let mut dom = place();
    let inputs = inputs()
        .with_module_source("ids", "return { Passes = { VIP = 1 } }")
        .with_module_source("env", "return { Name = \"dev\" }");

    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.GameIds","properties":{"Source":"$rbxsync_module"}},
                {"robloxPath":"ReplicatedStorage.Env","properties":{"Source":"$module:env"}}
            ]}"#,
        ),
        &inputs,
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    // `$rbxsync_module` is just the module named `ids` under its old spelling.
    assert!(source(&dom, "ReplicatedStorage.GameIds").contains("VIP"));
    assert!(source(&dom, "ReplicatedStorage.Env").contains("dev"));
}

/// The config says what it needs, so the CLI can refuse before touching the
/// place instead of emitting warnings that look like drift.
#[test]
fn a_config_reports_the_inputs_it_needs() {
    let needs = config(
        r#"{"injections":[
            {"robloxPath":"a.b","properties":{"Image":"ui.Icon"}},
            {"robloxPath":"a.c","properties":{"Source":"$module"}},
            {"robloxPath":"a.d","properties":{"Source":"$module:env"}},
            {"robloxPath":"a.e","properties":{"Source":"$rbxsync_module"}}
        ]}"#,
    )
    .needs();

    assert!(needs.asset_map);
    assert_eq!(
        needs.modules.iter().map(String::as_str).collect::<Vec<_>>(),
        ["assets", "env", "ids"]
    );

    // Literals need nothing at all, so a config of only literals must not
    // demand an asset map it never reads.
    let literals = config(
        r#"{"injections":[{"robloxPath":"a.b","keys":{"Volume":"$0.5","Name":"$$x"}}]}"#,
    )
    .needs();
    assert!(!literals.asset_map);
    assert!(literals.modules.is_empty());
}

#[test]
fn require_stub_points_at_the_uploaded_model() {
    let mut dom = place();
    apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.Main","properties":{"Source":"$require:models.main"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert_eq!(
        source(&dom, "ReplicatedStorage.Main"),
        "return require(333)\n"
    );
}

/// A path typo must not grow a Folder beside the real service and inject into
/// it: that reads as success and ships a place where nothing is wired up.
#[test]
fn an_unknown_service_is_refused_not_created() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorge.Assets","properties":{"Source":"$module"}}
            ]}"#,
        ),
        &inputs().with_module_source("assets", "return {}"),
    );

    assert!(rbx_inject::dom::find(&dom, "ReplicatedStorge").is_none());
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].contains("not a service"));
}

#[test]
fn a_missing_instance_warns_and_leaves_the_place_alone() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Nope","properties":{"Image":"ui.ShopIcon"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert!(!report.changed());
    assert_eq!(report.warnings.len(), 1);
}

#[test]
fn a_missing_asset_key_warns_and_names_the_key() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.Typo"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert!(!report.changed());
    assert!(report.warnings[0].contains("ui.Typo"), "{:?}", report.warnings);
}

// ─── Keys ────────────────────────────────────────────────────

#[test]
fn keys_edit_nested_values_and_keep_the_rest() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[{"robloxPath":"ReplicatedStorage.GameConfig","keys":{
                "Sounds.Bang": "audio.Bang",
                "Settings.Volume": "$0.5",
                "Settings.Debug": "$true",
                "GameName": "$$My Game"
            }}]}"#,
        ),
        &inputs(),
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let out = source(&dom, "ReplicatedStorage.GameConfig");
    assert!(out.contains(r#"Bang = "rbxassetid://222""#), "{out}");
    assert!(out.contains("Volume = 0.5"), "{out}");
    assert!(out.contains("Debug = true"), "{out}");
    assert!(out.contains(r#"GameName = "My Game""#), "{out}");
}

#[test]
fn a_rewritten_module_is_still_valid_luau() {
    let mut dom = place();
    apply(
        &mut dom,
        &config(
            r#"{"injections":[{"robloxPath":"ReplicatedStorage.GameConfig","keys":{
                "Sounds.Bang": "audio.Bang",
                "Weird.$4": "$$four",
                "Weird.has space": "$$spaced",
                "Weird.end": "$$keyword"
            }}]}"#,
        ),
        &inputs(),
    );

    // Feed the output back through the same evaluator: if the printer emitted a
    // bare `end =` or an unquoted key with a space, this fails.
    let out = source(&dom, "ReplicatedStorage.GameConfig");
    let reparsed = rbx_inject::luau::apply_keys(&out, &[]).expect("output should be valid Luau");
    assert!(reparsed.contains(r#"["has space"] = "spaced""#), "{reparsed}");
    assert!(reparsed.contains(r#"["end"] = "keyword""#), "{reparsed}");
    assert!(reparsed.contains(r#"[4] = "four""#), "{reparsed}");
}

#[test]
fn nil_removes_a_key() {
    let mut dom = place();
    apply(
        &mut dom,
        &config(
            r#"{"injections":[{"robloxPath":"ReplicatedStorage.GameConfig",
                "keys":{"Settings.Volume":"$nil"}}]}"#,
        ),
        &inputs(),
    );

    assert!(!source(&dom, "ReplicatedStorage.GameConfig").contains("Volume"));
}

#[test]
fn keys_on_a_non_module_warn_rather_than_corrupt() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[{"robloxPath":"StarterGui.Icon","keys":{"a":"$1"}}]}"#,
        ),
        &inputs(),
    );

    assert!(!report.changed());
    assert!(report.warnings[0].contains("ModuleScript"), "{:?}", report.warnings);
}

/// Properties run before keys for every rule, so a module whose whole Source is
/// replaced can still have keys edited inside the new source.
#[test]
fn a_replaced_source_is_what_keys_edit() {
    let mut dom = place();
    let inputs = inputs().with_module_source("assets", "return { Volume = 1, Name = \"stale\" }");

    apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.GameConfig","properties":{"Source":"$module"}},
                {"robloxPath":"ReplicatedStorage.GameConfig","keys":{"Name":"$$fresh"}}
            ]}"#,
        ),
        &inputs,
    );

    let out = source(&dom, "ReplicatedStorage.GameConfig");
    assert!(out.contains(r#"Name = "fresh""#), "{out}");
    assert!(out.contains("Volume = 1"), "{out}");
}

/// Re-running with unchanged inputs must produce the same bytes, or every deploy
/// shows a diff and nobody reads them any more.
#[test]
fn a_second_run_changes_nothing() {
    let rules = r#"{"injections":[{"robloxPath":"ReplicatedStorage.GameConfig","keys":{
        "Sounds.Bang": "audio.Bang", "Settings.Volume": "$0.5"
    }}]}"#;

    let mut dom = place();
    apply(&mut dom, &config(rules), &inputs());
    let first = source(&dom, "ReplicatedStorage.GameConfig");

    apply(&mut dom, &config(rules), &inputs());
    let second = source(&dom, "ReplicatedStorage.GameConfig");

    assert_eq!(first, second);
}

// ─── Config and asset map ────────────────────────────────────

/// The migration proof: `rbx-inject migrate` may not change what a config means.
#[test]
fn the_same_rules_in_json_and_toml_mean_the_same_thing() {
    let json = config(
        r#"{"injections":[
            {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.ShopIcon"}},
            {"robloxPath":"ReplicatedStorage.GameConfig","keys":{"Sounds.Bang":"audio.Bang"}}
        ]}"#,
    );

    let toml_text = json.to_toml().expect("should render");
    let from_toml: Config = toml::from_str(&toml_text).expect("should re-parse");

    let mut a = place();
    let mut b = place();
    apply(&mut a, &json, &inputs());
    apply(&mut b, &from_toml, &inputs());

    assert_eq!(
        source(&a, "ReplicatedStorage.GameConfig"),
        source(&b, "ReplicatedStorage.GameConfig")
    );
    assert_eq!(
        prop(&a, "StarterGui.Icon", "Image"),
        prop(&b, "StarterGui.Icon", "Image")
    );
}

/// The old line-by-line parser stopped at two levels. Asphalt may not emit
/// deeper today, but a user writing the config can, and it should resolve.
#[test]
fn the_asset_map_flattens_to_any_depth() {
    let mut dom = place();
    let report = apply(
        &mut dom,
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Image":"deep.nested.value"}}
            ]}"#,
        ),
        &inputs(),
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    match prop(&dom, "StarterGui.Icon", "Image") {
        Variant::ContentId(id) => assert_eq!(id.as_str(), "rbxassetid://444"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn an_empty_report_means_an_untouched_place() {
    let mut dom = place();
    let report: Report = apply(&mut dom, &Config::default(), &inputs());
    assert!(!report.changed());
    assert!(report.warnings.is_empty());
}
