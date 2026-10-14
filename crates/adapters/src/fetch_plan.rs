//! Planning and bounded-parallel execution of Binance historical fetch jobs.
//!
//! Each job targets an independent archive (one month or one day) and writes its
//! own Parquet file, so jobs can run concurrently without affecting the output.

use chrono::{Datelike, NaiveDate};

use crate::error::AdapterError;

/// One archive to download: a full month, or a single day when `day` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchTask {
    /// Calendar month (1-12).
    pub month: u32,
    /// Day of month for daily archives; `None` for the monthly archive.
    pub day: Option<u32>,
}

impl FetchTask {
    fn monthly(month: u32) -> Self {
        Self { month, day: None }
    }

    fn daily(month: u32, day: u32) -> Self {
        Self {
            month,
            day: Some(day),
        }
    }
}

/// Plans the archives to fetch for `year`, an optional `month` and an optional `day`.
///
/// Past months use the monthly archive. The month containing `today` is not yet
/// archived monthly by Binance, so it is fetched as daily archives up to `today`.
pub fn plan_fetch_tasks(
    year: u32,
    month: Option<u32>,
    day: Option<u32>,
    today: NaiveDate,
) -> Vec<FetchTask> {
    let current_year = today.year() as u32;
    let current_month = today.month();
    let current_day = today.day();
    let is_current_month = |m: u32| year == current_year && m == current_month;

    match (month, day) {
        (Some(m), Some(d)) => vec![FetchTask::daily(m, d)],
        (Some(m), None) if is_current_month(m) => {
            (1..=current_day).map(|d| FetchTask::daily(m, d)).collect()
        }
        (Some(m), None) => vec![FetchTask::monthly(m)],
        (None, _) if year == current_year => (1..current_month)
            .map(FetchTask::monthly)
            .chain((1..=current_day).map(|d| FetchTask::daily(current_month, d)))
            .collect(),
        (None, _) => (1..=12).map(FetchTask::monthly).collect(),
    }
}

/// Returns `true` when a fetch failed because Binance has not archived the file yet.
pub fn is_not_archived(err: &AdapterError) -> bool {
    err.to_string().contains("HTTP 404")
}

/// Applies `job` to every item using at most `jobs` worker threads.
///
/// Results are returned in the same order as `items`, regardless of completion order.
pub fn run_bounded<T, R, F>(items: &[T], jobs: usize, job: F) -> Result<Vec<R>, AdapterError>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    use rayon::prelude::*;

    if jobs == 0 {
        return Err(AdapterError::General(
            "Concurrency must be at least 1 job".to_string(),
        ));
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|e| AdapterError::General(format!("Failed to build fetch thread pool: {e}")))?;
    Ok(pool.install(|| items.par_iter().map(&job).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid test date")
    }

    #[test]
    fn past_year_plans_twelve_monthly_archives() {
        let tasks = plan_fetch_tasks(2025, None, None, date(2026, 10, 14));
        assert_eq!(tasks, (1..=12).map(FetchTask::monthly).collect::<Vec<_>>());
    }

    #[test]
    fn current_year_plans_past_months_then_daily_current_month() {
        let tasks = plan_fetch_tasks(2026, None, None, date(2026, 3, 2));
        assert_eq!(
            tasks,
            vec![
                FetchTask::monthly(1),
                FetchTask::monthly(2),
                FetchTask::daily(3, 1),
                FetchTask::daily(3, 2),
            ]
        );
    }

    #[test]
    fn single_past_month_plans_one_monthly_archive() {
        let tasks = plan_fetch_tasks(2026, Some(1), None, date(2026, 10, 14));
        assert_eq!(tasks, vec![FetchTask::monthly(1)]);
    }

    #[test]
    fn current_month_plans_daily_archives_up_to_today() {
        let tasks = plan_fetch_tasks(2026, Some(10), None, date(2026, 10, 3));
        assert_eq!(
            tasks,
            vec![
                FetchTask::daily(10, 1),
                FetchTask::daily(10, 2),
                FetchTask::daily(10, 3),
            ]
        );
    }

    #[test]
    fn explicit_day_plans_one_daily_archive() {
        let tasks = plan_fetch_tasks(2026, Some(1), Some(15), date(2026, 10, 14));
        assert_eq!(tasks, vec![FetchTask::daily(1, 15)]);
    }

    #[test]
    fn not_archived_detects_http_404_only() {
        let missing = AdapterError::General(
            "Failed to download archive from x (HTTP 404 Not Found)".to_string(),
        );
        let forbidden = AdapterError::General(
            "Failed to download archive from x (HTTP 403 Forbidden)".to_string(),
        );
        assert!(is_not_archived(&missing));
        assert!(!is_not_archived(&forbidden));
    }

    #[test]
    fn run_bounded_preserves_input_order() {
        let items: Vec<u64> = (0..16).collect();
        // Earlier items sleep longer so they finish last.
        let out = run_bounded(&items, 4, |&i| {
            std::thread::sleep(Duration::from_millis(16 - i));
            i * 10
        })
        .expect("pool builds");
        assert_eq!(out, items.iter().map(|i| i * 10).collect::<Vec<_>>());
    }

    #[test]
    fn run_bounded_never_exceeds_job_limit() {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let items: Vec<u32> = (0..12).collect();
        run_bounded(&items, 3, |_| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            active.fetch_sub(1, Ordering::SeqCst);
        })
        .expect("pool builds");
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn run_bounded_keeps_going_after_a_failed_item() {
        let items = [1u32, 2, 3];
        let out = run_bounded(&items, 2, |&i| {
            if i == 2 {
                Err(AdapterError::General("HTTP 404 Not Found".to_string()))
            } else {
                Ok(i)
            }
        })
        .expect("pool builds");
        assert!(matches!(out[0], Ok(1)));
        assert!(matches!(&out[1], Err(e) if is_not_archived(e)));
        assert!(matches!(out[2], Ok(3)));
    }

    #[test]
    fn run_bounded_rejects_zero_jobs() {
        let items = [1u32];
        assert!(run_bounded(&items, 0, |&i| i).is_err());
    }
}
