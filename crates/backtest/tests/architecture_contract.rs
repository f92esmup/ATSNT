//! Hexagonal dependency rule between the workspace crates.
//!
//! The domain knows nothing; contracts (`ports`) and strategies know only the domain; adapters
//! implement contracts; the application crate (`backtest`) wires everything together.
//!
//! Checked here: the dependency direction between workspace crates; no I/O crate and no I/O
//! API in the core (`domain`, `strategies`, `ports`); ports hold only traits and plain data;
//! network clients stay in adapters (one documented exception); the library names adapters and
//! reads the wall clock only in composition; runners resolve strategies through the registry.
//! Not checked: determinism beyond the clock, and business logic placed in adapters or binaries,
//! which need review.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const WORKSPACE_CRATES: [&str; 5] = ["domain", "ports", "strategies", "adapters", "backtest"];

/// Crates of the pure core: no I/O, no network, no runtime.
const CORE_CRATES: [&str; 3] = ["domain", "strategies", "ports"];

/// Every crate listed in the `[dependencies]` section of `crates/<name>/Cargo.toml`.
fn dependencies(name: &str) -> BTreeSet<String> {
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
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let dependency = line
            .split(['=', '.', ' '])
            .next()
            .unwrap_or_default()
            .trim();
        if !dependency.is_empty() {
            found.insert(dependency.to_string());
        }
    }
    found
}

/// Workspace crates listed in the `[dependencies]` section of `crates/<name>/Cargo.toml`.
fn internal_dependencies(name: &str) -> BTreeSet<String> {
    dependencies(name)
        .into_iter()
        .filter(|dependency| WORKSPACE_CRATES.contains(&dependency.as_str()))
        .collect()
}

/// Production code of every source file of `crates/<name>/src`: the part before the unit-test
/// module (see [`production_part`]), since unit tests may use anything.
fn production_sources(name: &str) -> Vec<(PathBuf, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(name)
        .join("src");
    let mut paths = Vec::new();
    rust_sources(&root, &mut paths);
    paths
        .into_iter()
        // A file that is a test module of its parent (`mod foo_tests;`) is not production code.
        .filter(|path| {
            !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.ends_with("tests"))
        })
        .map(|path| {
            let content = fs::read_to_string(&path).expect("readable source");
            let production = production_part(&content).to_string();
            (path, production)
        })
        .collect()
}

/// The part of a source file before its unit-test module. The cut is the first `#[cfg(test)]`
/// that is followed by `mod`: a `#[cfg(test)]` on a single `use` or item must not hide the
/// production code after it.
fn production_part(content: &str) -> &str {
    const GATE: &str = "#[cfg(test)]";
    let mut from = 0;
    while let Some(found) = content[from..].find(GATE) {
        let at = from + found;
        let after = content[at + GATE.len()..].trim_start();
        if after.starts_with("mod ") {
            return &content[..at];
        }
        from = at + GATE.len();
    }
    content
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

/// The core never pulls in an async runtime, a network client or a storage format.
#[test]
fn the_core_crates_depend_on_no_io_crate() {
    let io_crates = [
        "tokio",
        "reqwest",
        "tokio-tungstenite",
        "futures-util",
        "parquet",
        "arrow-array",
        "arrow-schema",
        "zip",
        "ring",
        "tempfile",
    ];
    let mut offenders = Vec::new();
    for name in CORE_CRATES {
        for dependency in dependencies(name) {
            if io_crates.contains(&dependency.as_str()) {
                offenders.push(format!("{name} depends on {dependency}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the core must stay free of I/O crates:\n{}",
        offenders.join("\n")
    );
}

/// The application crate reaches the network through adapters only: it names no HTTP or socket
/// client, so an exchange call cannot bypass the gateway the adapters provide.
#[test]
fn the_application_crate_names_no_network_client() {
    let network = ["reqwest", "tokio_tungstenite", "futures_util"];
    let mut offenders = Vec::new();
    for (path, production) in production_sources("backtest") {
        for (number, line) in production.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(api) = network.iter().find(|api| trimmed.contains(*api)) {
                offenders.push(format!("{}:{}: {api}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "network clients belong in adapters:\n{}",
        offenders.join("\n")
    );
}

/// Domain, strategies and ports are pure: no file system, no sockets, no runtime, no HTTP.
#[test]
fn the_core_crates_perform_no_io() {
    let forbidden = ["std::fs", "std::net", "tokio", "reqwest"];
    let mut offenders = Vec::new();
    for name in CORE_CRATES {
        for (path, production) in production_sources(name) {
            for (number, line) in production.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                if let Some(api) = forbidden.iter().find(|api| trimmed.contains(*api)) {
                    offenders.push(format!("{}:{}: {api}", path.display(), number + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "I/O found in the core crates:\n{}",
        offenders.join("\n")
    );
}

/// Ports are contracts: traits plus the plain data they exchange. Behaviour (an `impl` block
/// or a free function) belongs to the domain or to an adapter.
#[test]
fn ports_declare_traits_and_plain_data_only() {
    let mut offenders = Vec::new();
    for (path, production) in production_sources("ports") {
        for (number, line) in production.lines().enumerate() {
            // Only module-level items: methods declared inside a trait are indented.
            let behaviour = [
                "impl",
                "fn ",
                "pub fn ",
                "pub(crate) fn ",
                "async fn ",
                "pub async fn ",
            ]
            .iter()
            .any(|item| line.starts_with(item));
            if behaviour {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "ports may declare only traits and plain data:\n{}",
        offenders.join("\n")
    );
}

/// Ports are contracts only: they name no exchange and no storage format.
#[test]
fn ports_name_no_exchange_or_storage_format() {
    const FORBIDDEN: [&str; 4] = ["Binance", "Kraken", "Parquet", "BigQuery"];
    let mut offenders = Vec::new();
    for (path, production) in production_sources("ports") {
        for (number, line) in production.lines().enumerate() {
            if let Some(word) = FORBIDDEN.iter().find(|word| line.contains(*word)) {
                offenders.push(format!("{}:{}: {word}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "ports must not name exchanges or formats:\n{}",
        offenders.join("\n")
    );
}

/// Money, prices and quantities are fixed-point in the domain: no float type appears in its
/// production code.
#[test]
fn the_domain_has_no_floating_point_types() {
    let mut offenders = Vec::new();
    for (path, production) in production_sources("domain") {
        for (number, line) in production.lines().enumerate() {
            let has_float = line
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|token| token == "f32" || token == "f64");
            if has_float {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the domain must use Decimal, not floats:\n{}",
        offenders.join("\n")
    );
}

/// The library and the core receive instants as parameters, so the rows of a run do not depend on
/// when they were replayed. Adapters and composition (the binaries and `main.rs`) read the wall
/// clock. `Instant` stays allowed: it measures durations and reaches no record.
#[test]
fn only_adapters_and_composition_read_the_wall_clock() {
    let wall_clock = ["SystemTime::now", "Utc::now", "Local::now"];
    let mut offenders = Vec::new();
    for name in ["domain", "ports", "strategies", "backtest"] {
        for (path, production) in production_sources(name) {
            if is_composition_root(&path) {
                continue;
            }
            for (number, line) in production.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                if let Some(api) = wall_clock.iter().find(|api| trimmed.contains(*api)) {
                    offenders.push(format!("{}:{}: {api}", path.display(), number + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the library takes instants as parameters, the wall clock is read in composition:\n{}",
        offenders.join("\n")
    );
}

/// Binaries and the `main.rs` entry point start runs, so they are composition.
fn is_composition_root(path: &Path) -> bool {
    let in_bin_dir = path
        .parent()
        .and_then(|dir| dir.file_name())
        .is_some_and(|dir| dir == "bin");
    in_bin_dir || named(path, "main.rs")
}

/// Whether `path` has the file name `file`.
fn named(path: &Path, file: &str) -> bool {
    path.file_name().is_some_and(|name| name == file)
}

/// The library names no adapter outside composition: the wiring module, the binaries and `main.rs`
/// build the concrete adapters, and the library's logic is written against the ports.
#[test]
fn the_library_names_no_adapter_outside_composition() {
    let mut offenders = Vec::new();
    for (path, production) in production_sources("backtest") {
        if is_composition_root(&path) || named(&path, "wiring.rs") {
            continue;
        }
        for (number, line) in production.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains("adapters::") {
                offenders.push(format!("{}:{}: {trimmed}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the library names no adapter; the wiring module builds them:\n{}",
        offenders.join("\n")
    );
}

/// Only two library files reach the wiring module: the crate root, which re-exports it, and the
/// paper session. The paper session runs on a spawned task, so it holds the simulated venue
/// concretely: a generic venue would have to prove its futures are `Send`.
#[test]
fn the_library_reaches_the_wiring_only_through_its_entries() {
    const ENTRIES: [&str; 2] = ["lib.rs", "paper.rs"];
    let mut offenders = Vec::new();
    for (path, production) in production_sources("backtest") {
        let is_entry = ENTRIES.iter().any(|entry| named(&path, entry));
        if is_composition_root(&path) || named(&path, "wiring.rs") || is_entry {
            continue;
        }
        for (number, line) in production.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains("wiring::") {
                offenders.push(format!("{}:{}: {trimmed}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "only the entries may reach the wiring module from the library:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn a_gate_on_one_item_does_not_hide_the_rest_of_the_file() {
    let source = "use a;\n#[cfg(test)]\nuse b;\npub fn kept() {}\n#[cfg(test)]\nmod tests {\n    fn hidden() {}\n}\n";
    let production = production_part(source);
    assert!(production.contains("pub fn kept"), "{production}");
    assert!(!production.contains("fn hidden"), "{production}");
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

    // Every registered id and alias, so a new strategy is guarded without editing this test.
    let names = strategies::registry::strategy_names();
    let mut offenders = Vec::new();
    for path in sources {
        let content = fs::read_to_string(&path).expect("readable source");
        // Unit tests may name strategies freely; the rule is about production code.
        let production = production_part(&content);
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
