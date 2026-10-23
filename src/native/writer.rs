use crate::Client;
use crate::error::Error;
use crate::insert_formatted::InsertFormatted;
use crate::native::{Block, Column, DataTypeNode, Layout, LayoutKind, varuint};
use bytes::{Bytes, BytesMut};
use std::num::Saturating;

#[cfg(all(test, feature = "lz4", feature = "test-util"))]
#[path = "writer_test.rs"]
mod tests;

const BUFFER_SIZE: usize = 1024 * 1024;

pub(crate) struct BlockWriter {
    insert: InsertFormatted,
    buf: BytesMut,
    sent_rows: Saturating<usize>,
}

impl BlockWriter {
    pub(crate) fn new(insert: InsertFormatted) -> Self {
        Self {
            insert,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            sent_rows: Saturating(0),
        }
    }

    pub(crate) fn insert_mut(&mut self) -> &mut InsertFormatted {
        &mut self.insert
    }

    #[cfg_attr(not(test), expect(unused))]
    pub(crate) fn expect_client(&self) -> &Client {
        self.insert.expect_client()
    }

    pub(crate) fn expect_client_mut(&mut self) -> &mut Client {
        self.insert.expect_client_mut()
    }

    pub(crate) async fn write(&mut self, block: &Block) -> Result<(), Error> {
        if block.num_rows() == 0 {
            return Err(Error::Other("attempting to write an empty block".into()));
        }

        // If canceled while writing a block, we have no way to recover.
        // We have to abort the request instead.
        let mut guard = WriteGuard {
            insert: &mut self.insert,
            buf: &mut self.buf,
            finished: false,
        };

        guard.write_varuint(block.columns.len()).await?;
        guard.write_varuint(block.num_rows).await?;

        for column in &block.columns {
            guard.write_column(column).await?;
        }

        guard.flush().await?;

        guard.finished = true;

        self.sent_rows += block.num_rows;

        Ok(())
    }

    pub(crate) async fn end(mut self) -> Result<(), Error> {
        let mut guard = WriteGuard {
            insert: &mut self.insert,
            buf: &mut self.buf,
            finished: false,
        };

        guard.flush().await?;

        guard.finished = true;

        drop(guard);

        tracing::record_all!(
            self.insert.span(),
            clickhouse.request.sent_rows = self.sent_rows.0,
        );

        self.insert.end().await
    }
}

struct WriteGuard<'a> {
    insert: &'a mut InsertFormatted,
    buf: &'a mut BytesMut,
    finished: bool,
}

impl WriteGuard<'_> {
    async fn write_column(&mut self, column: &Column) -> Result<(), Error> {
        self.write_varuint(column.name.len()).await?;
        self.send(column.name.as_bytes()).await?;

        // We have to format the data type to know exactly how long the string is,
        // though in most cases we can use a static string.
        let data_type_str = column.data_type.to_str();

        self.write_varuint(data_type_str.len()).await?;
        self.send(data_type_str.as_bytes()).await?;

        if matches!(column.data_type, DataTypeNode::AggregateFunction(_, _)) {
            match &column.layout.kind {
                LayoutKind::Variable { data, .. } => self.send_data(data).await?,
                LayoutKind::Chunked { chunks } => {
                    for chunk in chunks {
                        self.send_data(chunk).await?;
                    }
                }
                _ => return Err(Error::Other("invalid aggregate state layout".into())),
            }
            return Ok(());
        }

        self.write_layout(&column.layout).await
    }

    async fn write_layout(&mut self, layout: &Layout) -> Result<(), Error> {
        if let Some(nulls) = &layout.nulls {
            self.send(nulls).await?;
        }

        match &layout.kind {
            LayoutKind::Chunked { chunks } => {
                for chunk in chunks {
                    self.write_varuint(chunk.len()).await?;
                    self.send_data(chunk).await?;
                }
            }
            LayoutKind::Fixed { data, .. } => {
                self.send_data(data).await?;
            }
            LayoutKind::Variable { end_offsets, data } => {
                let mut start_offset = 0;

                // Convert from offsets *back* to lengths and subslices
                for &end_offset in end_offsets {
                    let len = end_offset
                        .checked_sub(start_offset)
                        .ok_or_else(|| Error::Other(format!("BUG: string length underflow in encoding block: {end_offset} - {start_offset}").into()))?;

                    self.write_varuint(len).await?;

                    if len >= BUFFER_SIZE {
                        self.send_data(&data.slice(start_offset..end_offset))
                            .await?;
                    } else {
                        self.send(&data[start_offset..end_offset]).await?;
                    }

                    start_offset = end_offset;
                }
            }
            LayoutKind::LowCardinality(_) => {
                return Err(Error::Other(
                    "inserting LowCardinality data not yet implemented".into(),
                ));
            }
            LayoutKind::Array {
                end_indices,
                elem_layout,
            } => {
                for index in end_indices {
                    let index = u64::try_from(*index).map_err(|_| {
                        Error::Other(format!("array end index out of range: {index}").into())
                    })?;
                    self.send(&index.to_le_bytes()).await?;
                }

                Box::pin(self.write_layout(elem_layout)).await?;
            }
            LayoutKind::Tuple { layouts } => {
                for layout in layouts {
                    Box::pin(self.write_layout(layout)).await?;
                }
            }
            LayoutKind::Map {
                key_val_layouts,
                end_indices,
            } => {
                for index in end_indices {
                    let index = u64::try_from(*index).map_err(|_| {
                        Error::Other(format!("map end index out of range: {index}").into())
                    })?;
                    self.send(&index.to_le_bytes()).await?;
                }

                Box::pin(self.write_layout(&key_val_layouts[0])).await?;
                Box::pin(self.write_layout(&key_val_layouts[1])).await?;
            }
        }

        Ok(())
    }

    async fn write_varuint(&mut self, value: usize) -> Result<(), Error> {
        if self.buf.len() > BUFFER_SIZE - 10 {
            self.flush().await?;
        }
        varuint::write(&mut *self.buf, value);
        Ok(())
    }

    async fn send(&mut self, mut data: &[u8]) -> Result<(), Error> {
        while !data.is_empty() {
            let len = data.len().min(BUFFER_SIZE - self.buf.len());
            self.buf.extend_from_slice(&data[..len]);
            data = &data[len..];

            if self.buf.len() == BUFFER_SIZE {
                self.flush().await?;
            }
        }
        Ok(())
    }

    async fn send_data(&mut self, data: &Bytes) -> Result<(), Error> {
        let mut offset = 0;
        if !self.buf.is_empty() {
            let len = data.len().min(BUFFER_SIZE - self.buf.len());
            self.send(&data[..len]).await?;
            offset = len;
        }
        while data.len() - offset >= BUFFER_SIZE {
            self.insert
                .send(data.slice(offset..offset + BUFFER_SIZE))
                .await?;
            offset += BUFFER_SIZE;
        }
        self.send(&data[offset..]).await
    }

    async fn flush(&mut self) -> Result<(), Error> {
        if !self.buf.is_empty() {
            self.insert.send_buffer(self.buf).await?;
        }

        Ok(())
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.insert.abort();
        }
    }
}
