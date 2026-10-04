use std::io::BufRead;
use std::str::FromStr;

use domain::{Side, Trade};
use rust_decimal::Decimal;

use crate::error::AdapterError;
use crate::traits::MarketDataStream;

/// Streaming parser for Binance `aggTrades` CSV dumps (`data.binance.vision`).
///
/// Efficiently reads line by line without loading the entire multi-gigabyte
/// dataset into memory.
#[derive(Debug)]
pub struct BinanceCsvReader<R> {
    reader: R,
    line_number: usize,
    buffer: String,
}

impl<R: BufRead> BinanceCsvReader<R> {
    /// Constructs a new reader over any buffered I/O stream (file, memory cursor, etc.).
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line_number: 0,
            buffer: String::with_capacity(128),
        }
    }

    /// Helper to parse a single CSV line into a normalized Trade domain object.
    fn parse_line(&self, line: &str) -> Result<Option<Trade>, AdapterError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        // Detect and skip header row if present
        if trimmed.starts_with("agg_trade_id") || trimmed.starts_with("id") {
            return Ok(None);
        }

        let parts: Vec<&str> = trimmed.split(',').collect();
        if parts.len() < 7 {
            return Err(AdapterError::MalformedLine {
                line: self.line_number,
                found: parts.len(),
            });
        }

        // Column 1: price
        let price = Decimal::from_str(parts[1]).map_err(|_| AdapterError::DecimalParseError {
            line: self.line_number,
            raw: parts[1].to_string(),
        })?;

        // Column 2: quantity
        let quantity =
            Decimal::from_str(parts[2]).map_err(|_| AdapterError::DecimalParseError {
                line: self.line_number,
                raw: parts[2].to_string(),
            })?;

        // Column 5: transact_time
        let timestamp = parts[5]
            .parse::<i64>()
            .map_err(|_| AdapterError::TimestampParseError {
                line: self.line_number,
                raw: parts[5].to_string(),
            })?;

        // Column 6: is_buyer_maker
        // If buyer is maker (true), taker was seller -> Market Sell.
        // If buyer is taker (false), taker was buyer -> Market Buy.
        let is_buyer_maker = matches!(parts[6].trim().to_lowercase().as_str(), "true" | "t" | "1");

        let side = if is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        };

        let trade = Trade::new(timestamp, price, quantity, side).map_err(|source| {
            AdapterError::Domain {
                line: self.line_number,
                source,
            }
        })?;

        Ok(Some(trade))
    }
}

impl<R: BufRead> MarketDataStream for BinanceCsvReader<R> {
    type Error = AdapterError;

    fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error> {
        loop {
            self.buffer.clear();
            self.line_number += 1;

            let bytes_read = self.reader.read_line(&mut self.buffer)?;
            if bytes_read == 0 {
                // EOF reached
                return Ok(None);
            }

            if let Some(trade) = self.parse_line(&self.buffer)? {
                return Ok(Some(trade));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::io::Cursor;

    #[test]
    fn parse_valid_headered_csv() {
        let csv_data = "\
agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker
26129,59800.50,0.012,50001,50001,1700000000123,true
26130,59801.00,0.050,50002,50003,1700000000234,false
";
        let mut reader = BinanceCsvReader::new(Cursor::new(csv_data));

        // Trade 1: is_buyer_maker = true -> Side::Sell
        let t1 = reader.next_trade().unwrap().unwrap();
        assert_eq!(t1.timestamp, 1700000000123);
        assert_eq!(t1.price, dec!(59800.50));
        assert_eq!(t1.quantity, dec!(0.012));
        assert_eq!(t1.side, Side::Sell);

        // Trade 2: is_buyer_maker = false -> Side::Buy
        let t2 = reader.next_trade().unwrap().unwrap();
        assert_eq!(t2.timestamp, 1700000000234);
        assert_eq!(t2.price, dec!(59801.00));
        assert_eq!(t2.quantity, dec!(0.050));
        assert_eq!(t2.side, Side::Buy);

        // EOF
        assert!(reader.next_trade().unwrap().is_none());
    }

    #[test]
    fn parse_unheadered_csv() {
        let csv_data = "1001,65000.00,1.5,10,11,1700000005000,false\n";
        let mut reader = BinanceCsvReader::new(Cursor::new(csv_data));

        let t = reader.next_trade().unwrap().unwrap();
        assert_eq!(t.price, dec!(65000.00));
        assert_eq!(t.quantity, dec!(1.5));
        assert_eq!(t.side, Side::Buy);
        assert!(reader.next_trade().unwrap().is_none());
    }

    #[test]
    fn parse_malformed_line_errors() {
        let bad_csv = "1001,65000.00,1.5\n"; // incomplete columns
        let mut reader = BinanceCsvReader::new(Cursor::new(bad_csv));
        assert!(reader.next_trade().is_err());
    }
}
