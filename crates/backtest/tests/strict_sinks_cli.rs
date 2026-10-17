//! End-to-end checks of `--strict-sinks` on the `backtest` and `run_hpo` binaries.
//!
//! Cloud endpoints are redirected to a local HTTP server that fails every request,
//! so no external network is used.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use adapters::BinanceParquetWriter;
use domain::{Side, Trade};
use rust_decimal::Decimal;

/// Local endpoint answering every request with one fixed HTTP status and recording request lines.
struct FailingServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FailingServer {
    /// Answers every request with `500 Internal Server Error`.
    fn start() -> Self {
        Self::with_status("500 Internal Server Error")
    }

    /// Answers every request with `status` (e.g. `"412 Precondition Failed"`).
    fn with_status(status: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test port");
        let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let mut content_length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err()
                        || header == "\r\n"
                        || header.is_empty()
                    {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
                seen.lock().expect("request log").push(request_line);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log").clone()
    }
}

/// Writes a deterministic oscillating trade tape where every trade forms one $100 dollar bar.
fn write_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("BTCUSDT-aggTrades-2026-01.parquet");
    let mut writer = BinanceParquetWriter::new(&path).expect("create parquet fixture");
    for i in 0..4_000i64 {
        let wave = Decimal::from((i % 200 - 100).abs());
        let price = Decimal::from(100) + wave / Decimal::from(10);
        let side = if i % 3 == 0 { Side::Sell } else { Side::Buy };
        let trade = Trade::new(1_767_225_600_000 + i * 1_000, price, Decimal::ONE, side)
            .expect("valid fixture trade");
        writer.write_trade(&trade).expect("write fixture trade");
    }
    writer.finish().expect("finish parquet fixture");
    path
}

fn offline_cloud_env(cmd: &mut Command, server: &FailingServer) {
    cmd.env("GCP_PROJECT_ID", "test-project")
        .env("GCP_AUTH_TOKEN", "test-token")
        .env("GCS_BUCKET_NAME", "test-bucket")
        .env("ATSNT_BIGQUERY_ENDPOINT", &server.base_url)
        .env("ATSNT_GCS_ENDPOINT", &server.base_url);
}

fn run_backtest(data: &Path, server: &FailingServer, strict: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backtest"));
    cmd.arg("--data")
        .arg(data)
        .args(["--dollar-bar", "100", "--gcp-bigquery"]);
    if strict {
        cmd.arg("--strict-sinks");
    }
    offline_cloud_env(&mut cmd, server);
    cmd.output().expect("run backtest binary")
}

fn run_hpo(data: &Path, config: &Path, server: &FailingServer, strict: bool) -> Output {
    run_hpo_with(data, config, server, strict, true)
}

/// The fixture is too short for the default minimum of trades per fold; these checks are
/// about the sinks, so every candidate may win.
fn run_hpo_with(
    data: &Path,
    config: &Path,
    server: &FailingServer,
    strict: bool,
    bigquery: bool,
) -> Output {
    run_hpo_requiring(data, config, server, strict, bigquery, 0)
}

fn run_hpo_requiring(
    data: &Path,
    config: &Path,
    server: &FailingServer,
    strict: bool,
    bigquery: bool,
    min_trades_per_fold: usize,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_run_hpo"));
    cmd.arg("--data")
        .arg(data)
        .arg("--output-config")
        .arg(config)
        .args([
            "--dollar-bar",
            "100",
            "--folds",
            "2",
            "--candidates",
            "2",
            "--embargo-bars",
            "5",
            "--gcs-upload",
            "--artifact-suffix",
            "job-42",
            "--min-trades-per-fold",
            &min_trades_per_fold.to_string(),
        ]);
    if bigquery {
        cmd.arg("--gcp-bigquery");
    }
    if strict {
        cmd.arg("--strict-sinks");
    }
    offline_cloud_env(&mut cmd, server);
    cmd.output().expect("run run_hpo binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn backtest_tolerates_bigquery_failure_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let server = FailingServer::start();
    let out = run_backtest(&data, &server, false);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    // Tolerant sinks keep the baseline behavior: log the HTTP error and continue.
    assert!(text(&out.stdout).contains("[GCP :: BigQuery ERROR]"));
    assert!(!text(&out.stdout).contains("[WARN] Failed to record backtest run"));
    assert!(
        !server.requests().is_empty(),
        "BigQuery endpoint was not called"
    );
}

#[test]
fn backtest_strict_sinks_exits_non_zero_on_bigquery_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let server = FailingServer::start();
    let out = run_backtest(&data, &server, true);
    assert!(!out.status.success(), "strict run must fail");
    assert!(text(&out.stdout).contains("[WARN] Failed to record backtest run"));
    assert!(text(&out.stdout).contains("[TIMING] phase=total"));
    assert!(text(&out.stderr).contains("sink operation(s) failed in strict mode"));
}

#[test]
fn run_hpo_strict_sinks_keeps_local_artifact_and_fails_after_upload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let config = dir.path().join("hpo_btcusdt_2026_01.json");
    let server = FailingServer::start();
    let out = run_hpo(&data, &config, &server, true);
    let stdout = text(&out.stdout);
    assert!(
        !out.status.success(),
        "strict run must fail; stdout: {stdout}"
    );
    assert!(
        config.exists(),
        "local artifact must be written before failing"
    );
    assert!(text(&out.stderr).contains("sink operation(s) failed in strict mode"));
    assert!(server
        .requests()
        .iter()
        .any(|r| r.contains("name=artifacts/hpo_btcusdt_2026_01-job-42.json")));
}

#[test]
fn run_hpo_tolerates_sink_failures_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let config = dir.path().join("hpo_btcusdt_2026_01.json");
    let server = FailingServer::start();
    let out = run_hpo(&data, &config, &server, false);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    assert!(config.exists());
}

#[test]
fn run_hpo_strict_retry_keeps_existing_artifact() {
    // A retried Batch task re-uploads the same object name; GCS answers 412 for the
    // create-only upload because the first attempt already stored the artifact.
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let config = dir.path().join("hpo_btcusdt_2026_01.json");
    let server = FailingServer::with_status("412 Precondition Failed");
    let out = run_hpo_with(&data, &config, &server, true, false);
    let stdout = text(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        text(&out.stderr)
    );
    assert!(stdout.contains("already exists; keeping the existing object"));
    assert!(server
        .requests()
        .iter()
        .any(|r| r.contains("name=artifacts/hpo_btcusdt_2026_01-job-42.json&ifGenerationMatch=0")));
}

#[test]
fn run_hpo_without_an_eligible_candidate_fails_without_writing_or_uploading_a_winner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = write_fixture(dir.path());
    let config = dir.path().join("hpo_btcusdt_2026_01.json");
    let server = FailingServer::start();

    let out = run_hpo_requiring(&data, &config, &server, true, false, 1_000_000);

    let stdout = text(&out.stdout);
    assert!(!out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("0 of 2 candidates reached 1000000 closed trades in every fold"));
    assert!(text(&out.stderr).contains("no candidate reached 1000000 closed trades"));
    assert!(
        !config.exists(),
        "no artifact may be written without a winner"
    );
    assert!(!server
        .requests()
        .iter()
        .any(|r| r.contains("name=artifacts/")));
}
