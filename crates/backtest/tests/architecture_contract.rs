//! Hexagonal dependency rule between the workspace crates.
//!
//! The domain knows nothing; contracts (`ports`) and strategies know only the domain; adapters
//! implement contracts; the application crate (`backtest`) wires everything together.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const WORKSPACE_CRATES: [&str; 5] = ["domain", "ports", "strategies", "adapters", "backtest"];

/// Workspace crates listed in the `[dependencies]` section of `crates/<name>/Cargo.toml`.
fn internal_dependencies(name: &str) -> BTreeSet<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(name)
        .join("Cargo.toml");
    let content = fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", manifest.display()));

    let mut in_dependencies = false;
    let mut found = BTreeSet::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies {
            continue;
        }
        let dependency = line
            .split(['=', '.', ' '])
            .next()
            .unwrap_or_default()
            .trim();
        if WORKSPACE_CRATES.contains(&dependency) {
            found.insert(dependency.to_string());
        }
    }
    found
}

fn assert_depends_only_on(name: &str, allowed: &[&str]) {
    let actual = internal_dependencies(name);
    let allowed: BTreeSet<String> = allowed.iter().map(|s| s.to_string()).collect();
    assert!(
        actual.is_subset(&allowed),
        "`{name}` may depend only on {allowed:?} but depends on {actual:?}"
    );
}

#[test]
fn domain_depends_on_no_other_workspace_crate() {
    assert_depends_only_on("domain", &[]);
}

#[test]
fn ports_depend_only_on_the_domain() {
    assert_depends_only_on("ports", &["domain"]);
}

#[test]
fn strategies_depend_only_on_the_domain() {
    assert_depends_only_on("strategies", &["domain"]);
}

#[test]
fn adapters_depend_only_on_the_domain_and_the_ports() {
    assert_depends_only_on("adapters", &["domain", "ports"]);
}

#[test]
fn the_application_crate_may_wire_every_other_crate() {
    assert_depends_only_on("backtest", &["domain", "ports", "strategies", "adapters"]);
}
