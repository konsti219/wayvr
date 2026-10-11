use std::{
    collections::VecDeque,
    io::{ErrorKind, Read},
    os::{fd::AsFd, unix::net::UnixStream},
};

use prost::Message;

use crate::subsystem::monado_metrics::proto;

pub struct MonadoMetricsFd {
    stream_reader: UnixStream,

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
        // Monado holds its own copy now; dropping ours lets us see EOF when it drops the sink.
        drop(stream_writer);

        Ok(Self {
            stream_reader,
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

    // called every frame; returns false once Monado has closed its end of the stream
    pub fn update(&mut self) -> bool {
        let mut buf = [0_u8; 4096];

        loop {
            match self.stream_reader.read(&mut buf) {
                Ok(0) => return false,
                Ok(byte_count) => {
                    self.read_buffer.extend_from_slice(&buf[..byte_count]);
                    self.drain_read_buffer();
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(e) => {
                    log::error!("Failed to read Monado metrics stream: {e}");
                    return false;
                }
            }
        }
    }
}
