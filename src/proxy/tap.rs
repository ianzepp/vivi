//! Finish the access log after the response body has been consumed.

use std::time::Instant;

use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::access::{self, AccessEvent, AccessLog};
use super::sse;
use super::usage::{self, Usage};

/// Wrap `response` so the access log is written when the body ends.
pub fn wrap_logged(
    response: Response,
    log: AccessLog,
    event: AccessEvent,
    started: Instant,
    usage: Usage,
) -> Response {
    let (parts, body) = response.into_parts();
    let stream = tap_body(body, log, event, started, usage);
    Response::from_parts(parts, Body::from_stream(stream))
}

fn tap_body(
    body: Body,
    log: AccessLog,
    event: AccessEvent,
    started: Instant,
    usage: Usage,
) -> impl futures_util::Stream<Item = Result<Bytes, axum::Error>> {
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        relay(body, tx, log, event, started, usage).await;
    });
    ReceiverStream::new(rx)
}

async fn relay(
    body: Body,
    tx: mpsc::Sender<Result<Bytes, axum::Error>>,
    log: AccessLog,
    event: AccessEvent,
    started: Instant,
    usage: Usage,
) {
    let mut harvest = Harvest::default();
    let mut incoming = body.into_data_stream();
    while let Some(chunk) = incoming.next().await {
        match chunk {
            Ok(bytes) => {
                harvest.push(&bytes);
                if tx.send(Ok(bytes)).await.is_err() {
                    break;
                }
            }
            Err(error) => {
                if tx.send(Err(error)).await.is_err() {
                    break;
                }
            }
        }
    }
    write_finished(&log, event, started, usage, harvest.usage());
}

fn write_finished(
    log: &AccessLog,
    mut event: AccessEvent,
    started: Instant,
    mut usage: Usage,
    harvested: Usage,
) {
    usage.merge(harvested);
    event.total_ms = access::elapsed_ms(started);
    event.input_tokens = usage.input;
    event.output_tokens = usage.output;
    event.cached_tokens = usage.cached;
    event.upstream_id = usage.upstream_id;
    log.write(&event);
}

#[derive(Default)]
struct Harvest {
    full: Vec<u8>,
    sse_buf: Vec<u8>,
    usage: Usage,
}

impl Harvest {
    fn push(&mut self, bytes: &[u8]) {
        self.full.extend_from_slice(bytes);
        self.sse_buf.extend_from_slice(bytes);
        for event in sse::take_events(&mut self.sse_buf) {
            self.ingest(&event);
        }
    }

    fn ingest(&mut self, event: &sse::Event) {
        if let Some(data) = event.data()
            && let Some(found) = usage::from_sse_data(&data)
        {
            self.usage.merge(found);
            return;
        }
        if let Some(found) = usage::from_sse_data(event.raw.trim()) {
            self.usage.merge(found);
        }
    }

    fn usage(self) -> Usage {
        let mut usage = self.usage;
        if let Ok(value) = serde_json::from_slice(&self.full) {
            usage.merge(usage::from_value(&value));
        }
        usage
    }
}
