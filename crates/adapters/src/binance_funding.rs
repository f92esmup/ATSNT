//! Binance funding-rate history in the `GET /fapi/v1/fundingRate` response format.

use domain::futures::FundingSchedule;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

use crate::error::AdapterError;

#[derive(Debug, Deserialize)]
struct FundingRecord {
    #[serde(rename = "fundingTime")]
    funding_time: i64,
    #[serde(rename = "fundingRate")]
    funding_rate: String,
}

/// Parses a funding history JSON array into `(funding_time_ms, rate)` points.
pub fn parse_funding_history(json: &str) -> Result<Vec<(i64, Decimal)>, AdapterError> {
    let records: Vec<FundingRecord> = serde_json::from_str(json)?;
    records
        .into_iter()
        .map(|record| {
            Decimal::from_str(&record.funding_rate)
                .map(|rate| (record.funding_time, rate))
                .map_err(|error| {
                    AdapterError::General(format!(
                        "invalid funding rate {}: {error}",
                        record.funding_rate
                    ))
                })
        })
        .collect()
}

/// A schedule that charges the recorded rates and `default_rate` where none was recorded.
pub fn funding_schedule_from_history(
    json: &str,
    default_rate: Decimal,
) -> Result<FundingSchedule, AdapterError> {
    Ok(FundingSchedule::constant(default_rate).with_recorded(parse_funding_history(json)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::futures::FUNDING_INTERVAL_MS;
    use rust_decimal_macros::dec;

    const HISTORY: &str = r#"[
        {"symbol":"BTCUSDT","fundingTime":28800005,"fundingRate":"-0.00030000","markPrice":"100.5"},
        {"symbol":"BTCUSDT","fundingTime":57600000,"fundingRate":"0.00010000"}
    ]"#;

    #[test]
    fn the_exchange_response_parses_into_points() {
        let points = parse_funding_history(HISTORY).unwrap();

        assert_eq!(
            points,
            vec![(28_800_005, dec!(-0.0003)), (57_600_000, dec!(0.0001))]
        );
    }

    #[test]
    fn a_schedule_charges_the_recorded_rate_and_the_default_elsewhere() {
        let schedule = funding_schedule_from_history(HISTORY, dec!(0.0002)).unwrap();

        assert_eq!(schedule.rate_at(FUNDING_INTERVAL_MS), dec!(-0.0003));
        assert_eq!(schedule.rate_at(0), dec!(0.0002));
    }

    #[test]
    fn malformed_history_is_an_error() {
        assert!(parse_funding_history("not json").is_err());
        assert!(parse_funding_history(r#"[{"fundingTime":1,"fundingRate":"x"}]"#).is_err());
    }
}
