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

/// Serves `responses` (status line, raw body) in order, one per connection: archives are
/// binary, which the text-only `ScriptedServer` cannot send.
fn serve_bytes(responses: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local port");
    let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    base_url
}

/// A Binance-style monthly archive holding `csv`.
fn zip_archive(csv: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            "BTCUSDT-aggTrades-2024-01.csv",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start zip entry");
    writer.write_all(csv.as_bytes()).expect("write zip entry");
    writer.finish().expect("finish zip").into_inner()
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The success path: a verified archive still gets past the check and is converted.
#[test]
fn a_verified_archive_is_converted_to_parquet() {
    let archive = zip_archive(
        "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n\
         1,42000.5,0.010,1,1,1704067200000,true\n\
         2,42001.0,0.250,2,3,1704067200150,false\n",
    );
    let checksum = format!("{}  BTCUSDT-aggTrades-2024-01.zip\n", sha256_hex(&archive));
    let base_url = serve_bytes(vec![("200 OK", checksum.into_bytes()), ("200 OK", archive)]);
    let output = tempfile::tempdir().expect("output dir");
    let fetcher = BinanceDataFetcher::new()
        .with_progress(false)
        .with_base_url(base_url);
    let params = monthly_params(output.path());

    let summary = fetcher
        .fetch_and_convert(&params)
        .expect("a verified archive converts");

    assert_eq!(summary.total_trades, 2);
    assert!(params.target_parquet_path().exists());
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
