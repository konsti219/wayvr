use std::{
    collections::VecDeque,
    io::{ErrorKind, Read},
    os::{fd::AsFd, unix::net::UnixStream},
};

use prost::Message;

use crate::subsystem::monado_metrics::proto;

pub struct MonadoMetricsFd {
    stream_reader: UnixStream,
    #[allow(dead_code)]
    stream_writer: UnixStream,

    records: VecDeque<proto::Record>,
    read_buffer: Vec<u8>,
}

const RECORD_QUEUE_SIZE: usize = 500;

impl MonadoMetricsFd {
    pub fn new(monado: &mut libmonado::Monado) -> anyhow::Result<Self> {
        let (stream_reader, stream_writer) = std::os::unix::net::UnixStream::pair()?;
        stream_writer.set_nonblocking(true)?;
        stream_reader.set_nonblocking(true)?;

        monado.push_metrics_fd(stream_writer.as_fd(), true)?;

        Ok(Self {
            stream_reader,
            stream_writer,
            records: VecDeque::new(),
            read_buffer: Vec::new(),
        })
    }

    fn parse_message(&mut self, record: proto::Record) {
        self.records.push_back(record);
        if self.records.len() >= RECORD_QUEUE_SIZE {
            self.records.pop_front();
        }
    }

    pub fn dump_records(&mut self) -> Vec<proto::Record> {
        let records = std::mem::take(&mut self.records);
        records.into_iter().collect()
    }

    pub fn is_full(&self) -> bool {
        self.records.len() >= RECORD_QUEUE_SIZE - 1
    }

    fn drain_read_buffer(&mut self) {
        loop {
            let Ok(message_len) = prost::decode_length_delimiter(&self.read_buffer[..]) else {
                // The length prefix is a varint, so if 10 bytes still don't decode
                // we know the stream is malformed rather than merely incomplete.
                if self.read_buffer.len() >= 10 {
                    log::error!("Malformed Monado metrics length delimiter");
                    self.read_buffer.clear();
                }
                break;
            };

            let header_len = prost::length_delimiter_len(message_len);
            let total_len = header_len + message_len;
            if self.read_buffer.len() < total_len {
                break;
            }

            match proto::Record::decode(&self.read_buffer[header_len..total_len]) {
                Ok(record) => self.parse_message(record),
                Err(e) => {
                    log::error!("Monado metrics decode error: {e}");
                }
            }

            self.read_buffer.drain(..total_len);
        }
    }

    // called every frame
    pub fn update(&mut self) {
        let mut buf = [0_u8; 4096];

        loop {
            match self.stream_reader.read(&mut buf) {
                Ok(0) => {
                    debug_assert!(false);
                    break;
                }
                Ok(byte_count) => {
                    self.read_buffer.extend_from_slice(&buf[..byte_count]);
                    self.drain_read_buffer();
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    log::error!("Failed to read Monado metrics stream: {e}");
                    break;
                }
            }
        }
    }
}
