#![doc = include_str!("../README.md")]

use std::collections::HashMap;

use serde::ser::SerializeMap;
use serde::Serializer;
use tracing::{Event, Subscriber};
use tracing_serde::AsSerde;
use tracing_subscriber::{
    fmt::{format::Writer, FmtContext, FormatEvent, FormatFields, FormattedFields},
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
            let mut keys_and_values = HashMap::new();
            // The order of spans in the enumerator is from leaf to root. But we want to keep the values seen in the topmost spans.
            for (index, span) in scope.enumerate() {
                if index == 0 {
                    serializer_map.serialize_entry("span", span.name()).unwrap();
                }

                let ext = span.extensions();
                if let Some(data) = ext.get::<FormattedFields<N>>() {
                    if let serde_json::Value::Object(fields) =
                        serde_json::from_str::<serde_json::Value>(data).unwrap()
                    {
                        for (key, value) in fields {
                            keys_and_values.entry(key).or_insert(value);
                        }
                    }
                }
            }
            for (key, value) in keys_and_values {
                serializer_map.serialize_entry(&key, &value).unwrap();
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

    use pretty_assertions_sorted::assert_eq_sorted as assert_eq;
    use serde_json::value::Value;
    use tracing::{dispatcher, info, warn};
    use tracing_subscriber::{fmt::format::JsonFields, Layer, Registry};

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
            let span1 = tracing::info_span!("parent", x = 7, a = 1);
            let span2 = tracing::info_span!(parent: &span1, "child", y = 9, x = 6);

            let _s1 = span1.enter();
            let _s2 = span2.enter();

            info!(z = 10, "Test")
        });

        let data = writer.data.lock().unwrap();
        let data = std::str::from_utf8(&data).unwrap().trim();
        let fields = serde_json::from_str::<HashMap<String, serde_json::Value>>(data).unwrap();
        assert_eq!(
            fields,
            HashMap::from([
                ("level".to_string(), Value::String("INFO".to_string())),
                (
                    "target".to_string(),
                    Value::String("solink_tracing_flat_json::tests".to_string())
                ),
                ("message".to_string(), Value::String("Test".to_string())),
                ("z".to_string(), Value::Number(10.into())),
                ("span".to_string(), Value::String("child".to_string())),
                ("x".to_string(), Value::Number(6.into())),
                ("y".to_string(), Value::Number(9.into())),
                ("a".to_string(), Value::Number(1.into())),
            ])
        );
    }

    #[tokio::test]
    async fn should_keep_the_keys_from_leaf_spans() {
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
            span1.in_scope(|| {
                let span2 = tracing::info_span!("child1", y = 9, x = 1, z = 2);
                span2.in_scope(|| {
                    info!(t = 100, "Inside span1-span2");
                });

                let span3 = tracing::info_span!("leaf", a = 3, b = 4);
                span3.in_scope(|| {
                    warn!(t = 200, "Inside span1-span3");
                    let span4 = tracing::info_span!("child2", m = 5, n = 6);
                    span4.in_scope(|| {
                        info!(t = 300, v = 300, "Inside span1-span3-span4");
                    });
                });
            });

            info!(z = 10, "Test")
        });

        let data = writer.data.lock().unwrap();
        let data = std::str::from_utf8(&data).unwrap().trim();
        let lines = data
            .lines()
            .map(|line| serde_json::from_str::<HashMap<String, serde_json::Value>>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            HashMap::from([
                ("level".to_string(), Value::String("INFO".to_string())),
                (
                    "target".to_string(),
                    Value::String("solink_tracing_flat_json::tests".to_string())
                ),
                (
                    "message".to_string(),
                    Value::String("Inside span1-span2".to_string())
                ),
                ("t".to_string(), Value::Number(100.into())),
                ("span".to_string(), Value::String("child1".to_string())),
                ("y".to_string(), Value::Number(9.into())),
                ("x".to_string(), Value::Number(1.into())),
                ("z".to_string(), Value::Number(2.into())),
            ])
        );

        assert_eq!(
            lines[1],
            HashMap::from([
                ("level".to_string(), Value::String("WARN".to_string())),
                (
                    "target".to_string(),
                    Value::String("solink_tracing_flat_json::tests".to_string())
                ),
                (
                    "message".to_string(),
                    Value::String("Inside span1-span3".to_string())
                ),
                ("t".to_string(), Value::Number(200.into())),
                ("span".to_string(), Value::String("leaf".to_string())),
                ("a".to_string(), Value::Number(3.into())),
                ("b".to_string(), Value::Number(4.into())),
                ("x".to_string(), Value::Number(7.into())),
            ])
        );

        assert_eq!(
            lines[2],
            HashMap::from([
                ("level".to_string(), Value::String("INFO".to_string())),
                (
                    "target".to_string(),
                    Value::String("solink_tracing_flat_json::tests".to_string())
                ),
                (
                    "message".to_string(),
                    Value::String("Inside span1-span3-span4".to_string())
                ),
                ("t".to_string(), Value::Number(300.into())),
                ("v".to_string(), Value::Number(300.into())),
                ("span".to_string(), Value::String("child2".to_string())),
                ("m".to_string(), Value::Number(5.into())),
                ("n".to_string(), Value::Number(6.into())),
                ("a".to_string(), Value::Number(3.into())),
                ("b".to_string(), Value::Number(4.into())),
                ("x".to_string(), Value::Number(7.into())),
            ])
        );

        assert_eq!(
            lines[3],
            HashMap::from([
                ("level".to_string(), Value::String("INFO".to_string())),
                (
                    "target".to_string(),
                    Value::String("solink_tracing_flat_json::tests".to_string())
                ),
                ("message".to_string(), Value::String("Test".to_string())),
                ("z".to_string(), Value::Number(10.into())),
            ])
        );
    }
}
