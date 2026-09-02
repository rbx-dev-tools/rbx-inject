//! `check` is the drift alarm, so its tests are about what drifts.

use rbx_dom_weak::{InstanceBuilder, WeakDom};
use rbx_inject::assets::Assets;
use rbx_inject::check::{check, Severity};
use rbx_inject::config::Config;

fn place() -> WeakDom {
    WeakDom::new(
        InstanceBuilder::new("DataModel")
            .with_child(
                InstanceBuilder::new("ReplicatedStorage").with_child(
                    InstanceBuilder::new("ModuleScript")
                        .with_name("GameConfig")
                        .with_property("Source", "return { Volume = 1 }"),
                ),
            )
            .with_child(
                InstanceBuilder::new("StarterGui")
                    .with_child(InstanceBuilder::new("ImageLabel").with_name("Icon")),
            ),
    )
}

fn config(json: &str) -> Config {
    serde_json::from_str(json).expect("test config should parse")
}

fn assets() -> Assets {
    Assets::from_pairs([("ui.ShopIcon", "rbxassetid://111")])
}

fn errors(findings: &[rbx_inject::check::Finding]) -> Vec<String> {
    findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| format!("{f}"))
        .collect()
}

#[test]
fn a_place_matching_its_config_reports_nothing() {
    let findings = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.ShopIcon"}},
                {"robloxPath":"ReplicatedStorage.GameConfig","keys":{"Volume":"$0.5"}}
            ]}"#,
        ),
        Some(&assets()),
    );

    assert!(findings.is_empty(), "{findings:?}");
}

/// The failure this command exists for: somebody renames an instance in Studio,
/// and every rule underneath it silently stops applying.
#[test]
fn a_renamed_instance_is_an_error() {
    let findings = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.OldIconName","properties":{"Image":"ui.ShopIcon"}}
            ]}"#,
        ),
        Some(&assets()),
    );

    let errors = errors(&findings);
    assert_eq!(errors.len(), 1, "{findings:?}");
    assert!(errors[0].contains("no such instance"), "{errors:?}");
}

/// Without an asset map, path checking still works. That is what lets this run
/// before asphalt has uploaded anything.
#[test]
fn paths_are_checked_without_an_asset_map() {
    let findings = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Gone","properties":{"Image":"ui.ShopIcon"}}
            ]}"#,
        ),
        None,
    );

    assert_eq!(errors(&findings).len(), 1, "{findings:?}");
}

/// And the converse: a key that is only missing from the map must not be
/// reported when no map was given, or the pre-commit run is all noise.
#[test]
fn missing_asset_keys_are_only_checked_when_a_map_is_given() {
    let cfg = config(
        r#"{"injections":[
            {"robloxPath":"StarterGui.Icon","properties":{"Image":"ui.NotUploadedYet"}}
        ]}"#,
    );

    assert!(check(&place(), &cfg, None).is_empty());
    assert_eq!(errors(&check(&place(), &cfg, Some(&assets()))).len(), 1);
}

/// A property name Roblox does not have is written, then dropped on load, with
/// nothing anywhere saying so. Warning rather than error: the database lags the
/// newest Roblox release.
#[test]
fn a_misspelled_property_warns() {
    let findings = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"StarterGui.Icon","properties":{"Imagee":"ui.ShopIcon"}}
            ]}"#,
        ),
        Some(&assets()),
    );

    assert!(errors(&findings).is_empty(), "{findings:?}");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Warning);
    assert!(findings[0].message.contains("Imagee"), "{findings:?}");
}

/// A rule that writes a module source may create its target, so a missing path
/// is not a problem. A missing *service* still is.
#[test]
fn a_creatable_target_is_not_an_error_but_a_bad_service_is() {
    let ok = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorage.New.Generated","properties":{"Source":"$module"}}
            ]}"#,
        ),
        None,
    );
    assert!(ok.is_empty(), "{ok:?}");

    let bad = check(
        &place(),
        &config(
            r#"{"injections":[
                {"robloxPath":"ReplicatedStorge.Generated","properties":{"Source":"$module"}}
            ]}"#,
        ),
        None,
    );
    assert_eq!(errors(&bad).len(), 1, "{bad:?}");
    assert!(errors(&bad)[0].contains("not a service"));
}

#[test]
fn keys_on_a_non_module_are_an_error() {
    let findings = check(
        &place(),
        &config(r#"{"injections":[{"robloxPath":"StarterGui.Icon","keys":{"a":"$1"}}]}"#),
        None,
    );

    assert!(errors(&findings)[0].contains("ModuleScript"), "{findings:?}");
}

/// A module whose source does not evaluate to a table cannot be rewritten. Catch
/// it here rather than at deploy time.
#[test]
fn a_module_that_is_not_a_table_is_an_error() {
    let dom = WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("ReplicatedStorage").with_child(
                InstanceBuilder::new("ModuleScript")
                    .with_name("Logic")
                    .with_property("Source", "return function() return 1 end"),
            ),
        ),
    );

    let findings = check(
        &dom,
        &config(r#"{"injections":[{"robloxPath":"ReplicatedStorage.Logic","keys":{"a":"$1"}}]}"#),
        None,
    );

    assert_eq!(errors(&findings).len(), 1, "{findings:?}");
    assert!(errors(&findings)[0].contains("table"), "{findings:?}");
}
