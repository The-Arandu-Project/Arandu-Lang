//! JUnit XML serialization kept independent from process coordination.

use arandu_codegen::testing::{TestEventV1, TestStatus};

pub fn junit_report(events: &[TestEventV1], total_duration_ms: u128) -> String {
    let failures = events
        .iter()
        .filter(|event| event.status == TestStatus::Failed)
        .count();
    let errors = events
        .iter()
        .filter(|event| matches!(event.status, TestStatus::TimedOut | TestStatus::Crashed))
        .count();
    let skipped = events
        .iter()
        .filter(|event| event.status == TestStatus::Skipped)
        .count();
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites tests=\"{}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{}\">\n  <testsuite name=\"Arandu\" tests=\"{}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{}\">\n",
        events.len(),
        duration_seconds(total_duration_ms),
        events.len(),
        duration_seconds(total_duration_ms)
    );
    for event in events {
        let (classname, name) = event
            .id
            .rsplit_once("::")
            .unwrap_or(("arandu", event.id.as_str()));
        xml.push_str(&format!(
            "    <testcase classname=\"{}\" name=\"{}\" time=\"{}\">\n",
            xml_escape(classname),
            xml_escape(name),
            duration_seconds(event.duration.as_millis())
        ));
        match event.status {
            TestStatus::Passed => {}
            TestStatus::Skipped => xml.push_str("      <skipped/>\n"),
            TestStatus::Failed => push_failure(&mut xml, event),
            TestStatus::TimedOut | TestStatus::Crashed => push_error(&mut xml, event),
        }
        push_output(
            &mut xml,
            "system-out",
            &event.stdout.bytes,
            event.stdout.truncated,
        );
        push_output(
            &mut xml,
            "system-err",
            &event.stderr.bytes,
            event.stderr.truncated,
        );
        xml.push_str("    </testcase>\n");
    }
    xml.push_str("  </testsuite>\n</testsuites>\n");
    xml
}

fn push_failure(xml: &mut String, event: &TestEventV1) {
    let message = event
        .failure
        .as_ref()
        .map(|failure| failure.message.as_str())
        .unwrap_or("test failed");
    xml.push_str(&format!(
        "      <failure type=\"assertion\" message=\"{}\">{}</failure>\n",
        xml_escape(message),
        xml_escape(&format_test_failures(event))
    ));
}

fn push_error(xml: &mut String, event: &TestEventV1) {
    let kind = if event.status == TestStatus::TimedOut {
        "timeout"
    } else {
        "crash"
    };
    let message = event
        .failure
        .as_ref()
        .map(|failure| failure.message.as_str())
        .unwrap_or(kind);
    xml.push_str(&format!(
        "      <error type=\"{kind}\" message=\"{}\">{}</error>\n",
        xml_escape(message),
        xml_escape(&format_test_failures(event))
    ));
}

fn push_output(xml: &mut String, element: &str, bytes: &[u8], truncated: bool) {
    let output = String::from_utf8_lossy(bytes);
    if output.is_empty() && !truncated {
        return;
    }
    let suffix = if truncated {
        "\n<arandu: output truncated>"
    } else {
        ""
    };
    xml.push_str(&format!(
        "      <{element}>{}{}</{element}>\n",
        xml_escape(&output),
        xml_escape(suffix)
    ));
}

fn format_test_failures(event: &TestEventV1) -> String {
    let mut lines = Vec::new();
    if let Some(failure) = &event.failure {
        lines.push(failure.message.clone());
        if let Some(location) = &failure.location {
            lines.push(format!("location: {location}"));
        }
        if let Some(expected) = &failure.expected {
            lines.push(format!("expected: {expected}"));
        }
        if let Some(actual) = &failure.actual {
            lines.push(format!("actual: {actual}"));
        }
    }
    lines.extend(
        event
            .secondary_failures
            .iter()
            .map(|failure| format!("secondary: {}", failure.message)),
    );
    lines.extend(event.logs.iter().map(|line| format!("log: {line}")));
    if event.logs_truncated {
        lines.push("log: <truncated>".to_string());
    }
    lines.join("\n")
}

fn duration_seconds(milliseconds: u128) -> String {
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            character if character >= '\u{20}' => escaped.push(character),
            _ => escaped.push('\u{fffd}'),
        }
    }
    escaped
}
