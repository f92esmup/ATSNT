use strategies::DollarBarsCusumSpace;

#[test]
fn shipped_daytrading_cusum_param_space_matches_the_default_dense_grid() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../configs/param_space_daytrading_cusum.json"
    );
    let content = std::fs::read_to_string(path).expect("param space file");

    let space: DollarBarsCusumSpace = serde_json::from_str(&content).expect("valid space JSON");

    assert_eq!(space, DollarBarsCusumSpace::default());
}
