use std::fs::File;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use arrow_array::{Array, BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use domain::{Side, Trade};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use rust_decimal::Decimal;

use crate::error::AdapterError;
use crate::traits::MarketDataStream;

/// Default row group / batch size for Parquet streaming
const BATCH_SIZE: usize = 65_536;

/// Canonical schema for Binance `aggTrades` in Parquet.
pub fn agg_trades_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::Int64, false),
        Field::new("price", DataType::Utf8, false),
        Field::new("quantity", DataType::Utf8, false),
        Field::new("is_buyer_maker", DataType::Boolean, false),
    ]))
}

/// High-throughput Parquet writer for `Trade` streams.
///
/// Buffers trades and writes columnar `RecordBatch` chunks with Snappy compression.
pub struct BinanceParquetWriter {
    writer: ArrowWriter<File>,
    timestamps: Vec<i64>,
    prices: Vec<String>,
    quantities: Vec<String>,
    is_buyer_makers: Vec<bool>,
    total_written: usize,
}

impl BinanceParquetWriter {
    /// Creates a new Parquet writer targeting the specified file path.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, AdapterError> {
        let file = File::create(path)?;
        let schema = agg_trades_schema();
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let writer = ArrowWriter::try_new(file, schema, Some(props))
            .map_err(|e| AdapterError::General(format!("Failed to create Parquet writer: {e}")))?;

        Ok(Self {
            writer,
            timestamps: Vec::with_capacity(BATCH_SIZE),
            prices: Vec::with_capacity(BATCH_SIZE),
            quantities: Vec::with_capacity(BATCH_SIZE),
            is_buyer_makers: Vec::with_capacity(BATCH_SIZE),
            total_written: 0,
        })
    }

    /// Appends a domain `Trade` to the buffered columnar batch.
    pub fn write_trade(&mut self, trade: &Trade) -> Result<(), AdapterError> {
        self.timestamps.push(trade.timestamp);
        self.prices.push(trade.price.to_string());
        self.quantities.push(trade.quantity.to_string());
        self.is_buyer_makers.push(trade.side == Side::Sell);

        if self.timestamps.len() >= BATCH_SIZE {
            self.flush_batch()?;
        }

        Ok(())
    }

    /// Appends raw trade fields directly (optimizes CSV streaming conversion without intermediate allocations).
    pub fn write_raw(
        &mut self,
        timestamp: i64,
        price: &str,
        quantity: &str,
        is_buyer_maker: bool,
    ) -> Result<(), AdapterError> {
        self.timestamps.push(timestamp);
        self.prices.push(price.to_string());
        self.quantities.push(quantity.to_string());
        self.is_buyer_makers.push(is_buyer_maker);

        if self.timestamps.len() >= BATCH_SIZE {
            self.flush_batch()?;
        }

        Ok(())
    }

    /// Flushes currently buffered columns into an Arrow `RecordBatch` and writes it to Parquet.
    fn flush_batch(&mut self) -> Result<(), AdapterError> {
        if self.timestamps.is_empty() {
            return Ok(());
        }

        let schema = agg_trades_schema();
        let count = self.timestamps.len();

        let ts_array = Arc::new(Int64Array::from(std::mem::replace(
            &mut self.timestamps,
            Vec::with_capacity(BATCH_SIZE),
        )));
        let price_array = Arc::new(StringArray::from(std::mem::replace(
            &mut self.prices,
            Vec::with_capacity(BATCH_SIZE),
        )));
        let qty_array = Arc::new(StringArray::from(std::mem::replace(
            &mut self.quantities,
            Vec::with_capacity(BATCH_SIZE),
        )));
        let buyer_maker_array = Arc::new(BooleanArray::from(std::mem::replace(
            &mut self.is_buyer_makers,
            Vec::with_capacity(BATCH_SIZE),
        )));

        let batch = RecordBatch::try_new(
            schema,
            vec![ts_array, price_array, qty_array, buyer_maker_array],
        )
        .map_err(|e| AdapterError::General(format!("Failed to build RecordBatch: {e}")))?;

        self.writer
            .write(&batch)
            .map_err(|e| AdapterError::General(format!("Failed to write RecordBatch: {e}")))?;

        self.total_written += count;
        Ok(())
    }

    /// Finalizes the Parquet file, writing footers and metadata.
    pub fn finish(mut self) -> Result<usize, AdapterError> {
        self.flush_batch()?;
        self.writer
            .close()
            .map_err(|e| AdapterError::General(format!("Failed to close Parquet writer: {e}")))?;
        Ok(self.total_written)
    }
}

/// High-throughput Parquet reader streaming `Trade` events.
///
/// Implements `MarketDataStream` to plug seamlessly into `BacktestEngine`.
pub struct BinanceParquetReader {
    reader: ParquetRecordBatchReader,
    current_batch: Option<RecordBatch>,
    row_idx: usize,
    batch_len: usize,
}

impl BinanceParquetReader {
    /// Opens a Parquet file for streaming execution.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, AdapterError> {
        let file = File::open(path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| AdapterError::General(format!("Failed to open Parquet reader: {e}")))?;
        let reader = builder
            .with_batch_size(BATCH_SIZE)
            .build()
            .map_err(|e| AdapterError::General(format!("Failed to build Parquet reader: {e}")))?;

        let mut instance = Self {
            reader,
            current_batch: None,
            row_idx: 0,
            batch_len: 0,
        };

        instance.load_next_batch()?;
        Ok(instance)
    }

    fn load_next_batch(&mut self) -> Result<(), AdapterError> {
        match self.reader.next() {
            Some(Ok(batch)) => {
                self.row_idx = 0;
                self.batch_len = batch.num_rows();
                self.current_batch = Some(batch);
                Ok(())
            }
            Some(Err(e)) => Err(AdapterError::General(format!(
                "Error reading Parquet batch: {e}"
            ))),
            None => {
                self.current_batch = None;
                self.row_idx = 0;
                self.batch_len = 0;
                Ok(())
            }
        }
    }
}

impl MarketDataStream for BinanceParquetReader {
    type Error = AdapterError;

    fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error> {
        loop {
            let Some(batch) = &self.current_batch else {
                return Ok(None);
            };

            if self.row_idx >= self.batch_len {
                self.load_next_batch()?;
                continue;
            }

            let ts_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| {
                    AdapterError::General("Schema error: column 0 is not Int64Array".to_string())
                })?;
            let price_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| {
                    AdapterError::General("Schema error: column 1 is not StringArray".to_string())
                })?;
            let qty_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| {
                    AdapterError::General("Schema error: column 2 is not StringArray".to_string())
                })?;
            let buyer_maker_col = batch
                .column(3)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| {
                    AdapterError::General("Schema error: column 3 is not BooleanArray".to_string())
                })?;

            let timestamp = ts_col.value(self.row_idx);
            let price_str = price_col.value(self.row_idx);
            let qty_str = qty_col.value(self.row_idx);
            let is_buyer_maker = buyer_maker_col.value(self.row_idx);

            self.row_idx += 1;

            let price =
                Decimal::from_str(price_str).map_err(|_| AdapterError::DecimalParseError {
                    line: self.row_idx,
                    raw: price_str.to_string(),
                })?;
            let quantity =
                Decimal::from_str(qty_str).map_err(|_| AdapterError::DecimalParseError {
                    line: self.row_idx,
                    raw: qty_str.to_string(),
                })?;

            let side = if is_buyer_maker {
                Side::Sell
            } else {
                Side::Buy
            };

            let trade = Trade::new(timestamp, price, quantity, side).map_err(|source| {
                AdapterError::Domain {
                    line: self.row_idx,
                    source,
                }
            })?;

            return Ok(Some(trade));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use tempfile::NamedTempFile;

    #[test]
    fn roundtrip_parquet_writer_reader() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let mut writer = BinanceParquetWriter::new(&path).unwrap();
        let t1 = Trade::new(1700000000001, dec!(60000.50), dec!(1.5), Side::Buy).unwrap();
        let t2 = Trade::new(1700000000002, dec!(60001.00), dec!(0.25), Side::Sell).unwrap();

        writer.write_trade(&t1).unwrap();
        writer.write_trade(&t2).unwrap();
        let count = writer.finish().unwrap();
        assert_eq!(count, 2);

        let mut reader = BinanceParquetReader::open(&path).unwrap();
        let read1 = reader.next_trade().unwrap().unwrap();
        assert_eq!(read1, t1);

        let read2 = reader.next_trade().unwrap().unwrap();
        assert_eq!(read2, t2);

        assert!(reader.next_trade().unwrap().is_none());
    }
}
