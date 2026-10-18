//! Integrity of downloaded Binance archives: every archive is checked against its published
//! `.CHECKSUM` (SHA-256) before it is converted, and a mismatch never leaves a Parquet behind.

use std::io::Write;

use super::*;
use crate::gcp::test_http::ScriptedServer;

/// SHA-256 of the three bytes `abc` (FIPS 180-2 test vector).
const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn archive_with(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(bytes).expect("write archive");
    file
}

#[test]
fn accepts_an_archive_whose_digest_matches_the_published_checksum() {
    let archive = archive_with(b"abc");
    let checksum = format!("{ABC_SHA256}  BTCUSDT-aggTrades-2024-01.zip\n");

    verify_archive_sha256(archive.path(), &checksum).expect("matching digest passes");
}

#[test]
fn rejects_an_archive_whose_digest_differs_from_the_published_checksum() {
    let archive = archive_with(b"abd");
    let checksum = format!("{ABC_SHA256}  BTCUSDT-aggTrades-2024-01.zip\n");

    let error = verify_archive_sha256(archive.path(), &checksum).expect_err("digest differs");

    assert!(matches!(error, AdapterError::ChecksumMismatch { .. }));
}

#[test]
fn rejects_a_checksum_file_without_a_sha256_digest() {
    let archive = archive_with(b"abc");

    let error = verify_archive_sha256(archive.path(), "   \n").expect_err("no digest");
    assert!(matches!(error, AdapterError::ChecksumUnreadable(_)));

    let error = verify_archive_sha256(archive.path(), "not-hex  file.zip").expect_err("not hex");
    assert!(matches!(error, AdapterError::ChecksumUnreadable(_)));
}

fn monthly_params(output: &std::path::Path) -> FetchParams {
    FetchParams {
        symbol: "BTCUSDT".to_string(),
        year: 2024,
        month: 1,
        day: None,
        output_dir: output.to_path_buf(),
    }
}

/// An unpublished archive has no `.CHECKSUM` either: the 404 must still read as "not archived"
/// so `fetch_data` skips unpublished days, and the archive itself is never requested.
#[test]
fn a_missing_checksum_reads_as_not_archived_and_writes_nothing() {
    let server = ScriptedServer::start(&[("404 Not Found", "")]);
    let output = tempfile::tempdir().expect("output dir");
    let fetcher = BinanceDataFetcher::new().with_base_url(server.base_url.clone());
    let params = monthly_params(output.path());

    let error = fetcher
        .fetch_and_convert(&params)
        .expect_err("no checksum, no conversion");

    assert!(crate::fetch_plan::is_not_archived(&error), "{error}");
    assert!(!params.target_parquet_path().exists());
    let requests = server.request_lines();
    assert_eq!(
        requests.len(),
        1,
        "only the checksum is requested: {requests:?}"
    );
    assert!(requests[0].contains(".zip.CHECKSUM"), "{requests:?}");
}

/// Any other failure to fetch the checksum is a real failure, never a skipped archive.
#[test]
fn a_refused_checksum_is_a_failure_not_a_skip() {
    let server = ScriptedServer::start(&[("403 Forbidden", "")]);
    let output = tempfile::tempdir().expect("output dir");
    let fetcher = BinanceDataFetcher::new().with_base_url(server.base_url.clone());
    let params = monthly_params(output.path());

    let error = fetcher
        .fetch_and_convert(&params)
        .expect_err("a refused checksum fails the archive");

    assert!(!crate::fetch_plan::is_not_archived(&error), "{error}");
    assert!(!params.target_parquet_path().exists());
}

#[test]
fn a_corrupt_archive_fails_before_any_parquet_is_written() {
    // The fetcher downloads the `.CHECKSUM` first, then the archive (n-th request, n-th reply).
    let server = ScriptedServer::start(&[
        (
            "200 OK",
            "0000000000000000000000000000000000000000000000000000000000000000  file.zip\n",
        ),
        ("200 OK", "abc"),
    ]);
    let output = tempfile::tempdir().expect("output dir");
    let fetcher = BinanceDataFetcher::new().with_base_url(server.base_url.clone());
    let params = FetchParams {
        symbol: "BTCUSDT".to_string(),
        year: 2024,
        month: 1,
        day: None,
        output_dir: output.path().to_path_buf(),
    };

    let error = fetcher
        .fetch_and_convert(&params)
        .expect_err("a digest mismatch must fail the archive");

    assert!(matches!(error, AdapterError::ChecksumMismatch { .. }));
    assert!(!params.target_parquet_path().exists());
}
