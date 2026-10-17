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

/// Source files under `dir`, recursively.
fn rust_sources(dir: &std::path::Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
    {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            found.push(path);
        }
    }
}

/// Runners select strategies through the registry, never by matching on their names, so adding a
/// strategy means one registry line and no edit to any runner.
#[test]
fn runners_do_not_dispatch_on_strategy_names() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);

    let names = ["dollar_bars_cusum", "cusum", "order_flow_imbalance", "ofi"];
    let mut offenders = Vec::new();
    for path in sources {
        let content = fs::read_to_string(&path).expect("readable source");
        // Unit tests may name strategies freely; the rule is about production code.
        let production = content.split("#[cfg(test)]").next().unwrap_or_default();
        for (number, line) in production.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            let dispatches = names.iter().any(|name| {
                let literal = format!("\"{name}\"");
                trimmed.contains(&format!("{literal} =>"))
                    || trimmed.contains(&format!("{literal} |"))
                    || trimmed.contains(&format!("| {literal}"))
                    || trimmed.contains(&format!("== {literal}"))
                    || trimmed.contains(&format!("!= {literal}"))
            });
            if dispatches {
                offenders.push(format!("{}:{}: {trimmed}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "strategy names must be resolved by the registry, not matched in runners:\n{}",
        offenders.join("\n")
    );
}
