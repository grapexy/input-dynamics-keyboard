use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::getevent::parser::{DeviceAdded, InputEvent, ParsedLine, parse_line};
use crate::getevent::touch_state::TouchState;
use crate::getevent::{NormalizeError, NormalizeResult};

/// Schema value written to normalized `getevent` JSONL records.
pub const GETEVENT_SCHEMA: &str = "input_dynamics_getevent.v1";

/// Summary of a `getevent` normalization run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NormalizeStats {
    /// Number of input lines read from the raw `getevent` stream.
    pub lines: u64,
    /// Number of JSONL records written.
    pub records: u64,
    /// Number of `device_added` records written.
    pub devices: u64,
    /// Number of parsed low-level input event records written.
    pub input_events: u64,
    /// Number of reconstructed touch frame records written.
    pub touch_frames: u64,
    /// Number of preserved lines that did not match known `getevent` formats.
    pub unparsed_lines: u64,
}

impl NormalizeStats {
    pub(crate) fn increment_line_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.lines)
    }

    fn increment_record_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.records)
    }

    fn increment_device_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.devices)
    }

    fn increment_input_event_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.input_events)
    }

    fn increment_touch_frame_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.touch_frames)
    }

    fn increment_unparsed_line_count(&mut self) -> NormalizeResult<()> {
        increment(&mut self.unparsed_lines)
    }
}

/// Normalize a raw `getevent -lt` file into JSONL.
pub fn normalize_file(input: &Path, output: &Path) -> NormalizeResult<NormalizeStats> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let input_file = File::open(input)?;
    let output_file = File::create(output)?;
    normalize_reader(BufReader::new(input_file), BufWriter::new(output_file))
}

/// Normalize a raw `getevent -lt` stream into JSONL.
pub fn normalize_reader<R, W>(reader: R, writer: W) -> NormalizeResult<NormalizeStats>
where
    R: BufRead,
    W: Write,
{
    let normalizer = Normalizer {
        writer,
        stats: NormalizeStats::default(),
        pending_device: None,
        touch_state: TouchState::default(),
    };
    normalizer.run(reader)
}

struct Normalizer<W> {
    writer: W,
    stats: NormalizeStats,
    pending_device: Option<PendingDevice>,
    touch_state: TouchState,
}

impl<W> Normalizer<W>
where
    W: Write,
{
    fn run<R>(mut self, reader: R) -> NormalizeResult<NormalizeStats>
    where
        R: BufRead,
    {
        for line_result in reader.lines() {
            let line = line_result?;
            self.stats.increment_line_count()?;
            let line_index = self.stats.lines;
            self.handle_line(line_index, &line)?;
        }
        self.flush_pending_device()?;
        self.writer.flush()?;
        Ok(self.stats)
    }

    fn handle_line(&mut self, line_index: u64, line: &str) -> NormalizeResult<()> {
        match parse_line(line)? {
            ParsedLine::Blank => {}
            ParsedLine::DeviceAdded(device) => {
                self.flush_pending_device()?;
                self.pending_device = Some(PendingDevice::new(line_index, device));
            }
            ParsedLine::DeviceName(device_name) => {
                if let Some(pending_device) = self.pending_device.take() {
                    self.write_device_added(pending_device.with_name(device_name))?;
                } else {
                    self.write_unparsed(line_index, line)?;
                }
            }
            ParsedLine::InputEvent(event) => {
                self.flush_pending_device()?;
                self.write_input_event(line_index, &event)?;
                if let Some(frame) = self.touch_state.update(&event, line_index)? {
                    self.write_touch_frame(&frame)?;
                }
            }
            ParsedLine::Unparsed(raw_line) => {
                self.flush_pending_device()?;
                self.write_unparsed(line_index, &raw_line)?;
            }
        }
        Ok(())
    }

    fn flush_pending_device(&mut self) -> NormalizeResult<()> {
        if let Some(pending_device) = self.pending_device.take() {
            self.write_device_added(pending_device)?;
        }
        Ok(())
    }

    fn write_device_added(&mut self, pending_device: PendingDevice) -> NormalizeResult<()> {
        let PendingDevice {
            line_index,
            device,
            device_name,
        } = pending_device;
        let DeviceAdded {
            device_index,
            event_path,
        } = device;
        let record = json!({
            "schema": GETEVENT_SCHEMA,
            "event": "device_added",
            "line_index": line_index,
            "device_index": device_index,
            "event_path": event_path,
            "device_name": device_name,
        });
        self.stats.increment_device_count()?;
        self.write_record(&record)
    }

    fn write_input_event(&mut self, line_index: u64, event: &InputEvent) -> NormalizeResult<()> {
        let record = json!({
            "schema": GETEVENT_SCHEMA,
            "event": "input_event",
            "line_index": line_index,
            "event_path": event.event_path,
            "t_getevent_seconds": event.timestamp.text,
            "t_getevent_us": event.timestamp.micros,
            "event_type": event.event_type,
            "code": event.code,
            "value_raw": event.value.raw,
            "value_i64": event.value.integer,
            "key_state": event.value.key_state,
        });
        self.stats.increment_input_event_count()?;
        self.write_record(&record)
    }

    fn write_touch_frame(&mut self, frame: &Value) -> NormalizeResult<()> {
        self.stats.increment_touch_frame_count()?;
        self.write_record(frame)
    }

    fn write_unparsed(&mut self, line_index: u64, raw_line: &str) -> NormalizeResult<()> {
        let record = json!({
            "schema": GETEVENT_SCHEMA,
            "event": "unparsed_line",
            "line_index": line_index,
            "raw_line": raw_line,
        });
        self.stats.increment_unparsed_line_count()?;
        self.write_record(&record)
    }

    fn write_record(&mut self, record: &Value) -> NormalizeResult<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.stats.increment_record_count()
    }
}

struct PendingDevice {
    line_index: u64,
    device: DeviceAdded,
    device_name: Option<String>,
}

impl PendingDevice {
    const fn new(line_index: u64, device: DeviceAdded) -> Self {
        Self {
            line_index,
            device,
            device_name: None,
        }
    }

    fn with_name(mut self, device_name: String) -> Self {
        self.device_name = Some(device_name);
        self
    }
}

fn increment(value: &mut u64) -> NormalizeResult<()> {
    *value = value
        .checked_add(1)
        .ok_or_else(|| NormalizeError::new("normalization count overflow"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::Value;

    use crate::getevent::normalize::normalize_reader;

    #[test]
    fn normalize_fixture_writes_input_events_and_touch_frames() {
        let fixture = include_str!("../../tests/fixtures/getevent/simple_tap.raw.log");
        let mut output = Vec::new();

        let stats = normalize_reader(Cursor::new(fixture), &mut output);

        assert!(stats.is_ok(), "fixture should normalize");
        assert_eq!(
            stats.ok().map(|value| value.touch_frames),
            Some(2),
            "fixture should produce down and up frames"
        );
        let output_text = String::from_utf8(output);
        assert!(output_text.is_ok(), "JSONL should be UTF-8");
        let records = parse_jsonl(&output_text.unwrap_or_default());
        assert!(
            records.iter().any(|record| {
                record.get("event").and_then(Value::as_str) == Some("device_added")
            }),
            "device record should be present"
        );
        assert!(
            records.iter().any(|record| {
                record.get("event").and_then(Value::as_str) == Some("input_event")
            }),
            "input event records should be present"
        );
        assert!(
            records.iter().any(|record| {
                record.get("event").and_then(Value::as_str) == Some("touch_frame")
            }),
            "touch frames should be present"
        );
    }

    fn parse_jsonl(text: &str) -> Vec<Value> {
        let mut values = Vec::new();
        for line in text.lines() {
            let parsed = serde_json::from_str::<Value>(line);
            assert!(parsed.is_ok(), "JSONL line should parse");
            if let Ok(value) = parsed {
                values.push(value);
            }
        }
        values
    }
}
