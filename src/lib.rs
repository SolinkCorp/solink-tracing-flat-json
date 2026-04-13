#![doc = include_str!("../README.md")]

use serde::Serializer;
use serde::ser::SerializeMap;
use tracing::{Event, Subscriber};
use tracing_serde::AsSerde;
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields, format::Writer},
    registry::LookupSpan,
};

/// `FormatEvent` for serializing data as JSON.
///
/// Adapted from the example in https://github.com/tokio-rs/tracing/issues/2670.
///
pub struct SolinkJsonFormat {
    add_timestamp: bool,
    add_target: bool,
}

impl SolinkJsonFormat {
    pub fn new() -> Self {
        Self {
            add_timestamp: true,
            add_target: true,
        }
    }

    /// Set whether to add a timestamp to the log.
    pub fn with_timestamp(mut self, add_timestamp: bool) -> Self {
        self.add_timestamp = add_timestamp;
        self
    }

    /// Set whether to add the target to the log.
    pub fn with_target(mut self, add_target: bool) -> Self {
        self.add_target = add_target;
        self
    }
}

impl Default for SolinkJsonFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, N> FormatEvent<S, N> for SolinkJsonFormat
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let meta = event.metadata();

        let mut s = Vec::<u8>::new();
        let mut serializer = serde_json::Serializer::new(&mut s);
        let mut serializer_map = serializer.serialize_map(None).unwrap();

        if self.add_timestamp {
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            serializer_map
                .serialize_entry("timestamp", &timestamp)
                .unwrap();
        }

        serializer_map
            .serialize_entry("level", &meta.level().as_serde())
            .unwrap();

        if self.add_target {
            serializer_map
                .serialize_entry("target", meta.target())
                .unwrap();
        }

        let mut visitor = tracing_serde::SerdeMapVisitor::new(serializer_map);
        event.record(&mut visitor);
        let mut serializer_map = visitor.take_serializer().unwrap();

        if let Some(scope) = ctx.event_scope() {
            for (index, span) in scope.enumerate() {
                if index == 0 {
                    serializer_map.serialize_entry("span", span.name()).unwrap();
                }

                let ext = span.extensions();
                if let Some(data) = ext.get::<FormattedFields<N>>()
                    && let serde_json::Value::Object(fields) =
                        serde_json::from_str::<serde_json::Value>(data).unwrap()
                {
                    for field in fields {
                        serializer_map.serialize_entry(&field.0, &field.1).unwrap();
                    }
                }
            }
        }

        serializer_map.end().unwrap();

        writer.write_str(std::str::from_utf8(&s).unwrap()).unwrap();
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {

    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use tracing::{dispatcher, info};
    use tracing_subscriber::{Layer, Registry, fmt::format::JsonFields};

    use super::*;

    #[derive(Debug, Clone)]
    struct TestWriter {
        data: Arc<Mutex<Vec<u8>>>,
    }

    impl TestWriter {
        fn new() -> Self {
            Self {
                data: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.data.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn should_write_a_log() {
        let writer = TestWriter::new();

        let log_to_file = {
            let writer = writer.clone();
            tracing_subscriber::fmt::layer()
                .event_format(SolinkJsonFormat::new().with_timestamp(false))
                .fmt_fields(JsonFields::default())
                .with_writer(move || writer.clone())
        };

        let subscriber = log_to_file.with_subscriber(Registry::default());
        let dispatch = dispatcher::Dispatch::new(subscriber);
        dispatcher::with_default(&dispatch, || {
            let span1 = tracing::info_span!("parent", x = 7);
            let span2 = tracing::info_span!(parent: &span1, "child", y = 9);

            let _s1 = span1.enter();
            let _s2 = span2.enter();

            info!(z = 10, "Test")
        });

        let data = writer.data.lock().unwrap();
        let data = std::str::from_utf8(&data).unwrap();
        assert_eq!(
            data.trim(),
            r#"{"level":"INFO","target":"solink_tracing_flat_json::tests","message":"Test","z":10,"span":"child","y":9,"x":7}"#,
        );
    }
}
