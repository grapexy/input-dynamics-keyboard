use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::getevent::NormalizeResult;
use crate::getevent::normalize::GETEVENT_SCHEMA;
use crate::getevent::parser::InputEvent;

#[derive(Default)]
pub(crate) struct TouchState {
    devices: BTreeMap<String, DeviceState>,
}

impl TouchState {
    pub(crate) fn update(
        &mut self,
        event: &InputEvent,
        line_index: u64,
    ) -> NormalizeResult<Option<Value>> {
        let device = self.devices.entry(event.event_path.clone()).or_default();
        device.update(event, line_index)
    }
}

#[derive(Default)]
struct DeviceState {
    current_slot: i64,
    next_sequence_index: u64,
    slots: BTreeMap<i64, SlotState>,
}

impl DeviceState {
    fn update(&mut self, event: &InputEvent, line_index: u64) -> NormalizeResult<Option<Value>> {
        match event.event_type.as_str() {
            "EV_ABS" => self.update_abs(event)?,
            "EV_KEY" => self.update_key(event)?,
            "EV_SYN" if event.code == "SYN_REPORT" => {
                return self.touch_frame(event, line_index);
            }
            _ => {}
        }
        Ok(None)
    }

    fn update_abs(&mut self, event: &InputEvent) -> NormalizeResult<()> {
        let Some(value) = event.value.integer else {
            return Ok(());
        };
        if event.code == "ABS_MT_SLOT" {
            self.current_slot = value;
            self.slot_mut(value);
            return Ok(());
        }
        let current_slot = self.current_slot;
        match event.code.as_str() {
            "ABS_MT_TRACKING_ID" => self.update_tracking_id(current_slot, value)?,
            "ABS_MT_POSITION_X" | "ABS_X" => self.slot_mut(current_slot).x = Some(value),
            "ABS_MT_POSITION_Y" | "ABS_Y" => self.slot_mut(current_slot).y = Some(value),
            "ABS_MT_PRESSURE" | "ABS_PRESSURE" => {
                self.slot_mut(current_slot).pressure = Some(value);
            }
            "ABS_MT_TOUCH_MAJOR" => self.slot_mut(current_slot).touch_major = Some(value),
            "ABS_MT_TOUCH_MINOR" => self.slot_mut(current_slot).touch_minor = Some(value),
            "ABS_MT_ORIENTATION" => self.slot_mut(current_slot).orientation = Some(value),
            _ => return Ok(()),
        }
        self.slot_mut(current_slot).flags.changed = true;
        Ok(())
    }

    fn update_key(&mut self, event: &InputEvent) -> NormalizeResult<()> {
        if event.code != "BTN_TOUCH" {
            return Ok(());
        }
        match event.value.key_state.as_deref() {
            Some("DOWN") => {
                let slot = self.slot_mut(0);
                if !slot.active && slot.tracking_id.is_none() {
                    self.start_slot(0, None)?;
                }
            }
            Some("UP") => {
                self.end_slot(0);
            }
            _ => {}
        }
        Ok(())
    }

    fn update_tracking_id(&mut self, slot_index: i64, tracking_id: i64) -> NormalizeResult<()> {
        if tracking_id == -1 {
            self.end_slot(slot_index);
        } else {
            self.start_slot(slot_index, Some(tracking_id))?;
        }
        Ok(())
    }

    fn start_slot(&mut self, slot_index: i64, tracking_id: Option<i64>) -> NormalizeResult<()> {
        self.next_sequence_index = self
            .next_sequence_index
            .checked_add(1)
            .ok_or_else(|| crate::getevent::NormalizeError::new("touch sequence index overflow"))?;
        let sequence_index = self.next_sequence_index;
        let slot = self.slot_mut(slot_index);
        slot.active = true;
        slot.flags.started = true;
        slot.flags.ended = false;
        slot.flags.changed = true;
        slot.sequence_index = Some(sequence_index);
        slot.tracking_id = tracking_id;
        Ok(())
    }

    fn end_slot(&mut self, slot_index: i64) {
        let slot = self.slot_mut(slot_index);
        if slot.active || slot.tracking_id.is_some() || slot.sequence_index.is_some() {
            slot.active = false;
            slot.flags.ended = true;
            slot.flags.changed = true;
        }
    }

    fn touch_frame(
        &mut self,
        event: &InputEvent,
        line_index: u64,
    ) -> NormalizeResult<Option<Value>> {
        let mut slot_values = Vec::new();
        let mut active_touch_count = 0_u64;
        for (slot_index, slot) in &self.slots {
            if slot.active {
                active_touch_count = active_touch_count.checked_add(1).ok_or_else(|| {
                    crate::getevent::NormalizeError::new("active touch count overflow")
                })?;
            }
            if slot.should_emit() {
                slot_values.push(slot.to_json(*slot_index));
            }
        }
        if slot_values.is_empty() {
            return Ok(None);
        }
        for slot in self.slots.values_mut() {
            slot.after_emit();
        }
        Ok(Some(json!({
            "schema": GETEVENT_SCHEMA,
            "event": "touch_frame",
            "line_index": line_index,
            "event_path": event.event_path,
            "t_getevent_seconds": event.timestamp.text,
            "t_getevent_us": event.timestamp.micros,
            "active_slot": self.current_slot,
            "active_touch_count": active_touch_count,
            "slots": slot_values,
        })))
    }

    fn slot_mut(&mut self, slot_index: i64) -> &mut SlotState {
        self.slots.entry(slot_index).or_default()
    }
}

#[derive(Default)]
struct SlotFlags {
    started: bool,
    ended: bool,
    changed: bool,
}

#[derive(Default)]
struct SlotState {
    active: bool,
    flags: SlotFlags,
    tracking_id: Option<i64>,
    sequence_index: Option<u64>,
    x: Option<i64>,
    y: Option<i64>,
    pressure: Option<i64>,
    touch_major: Option<i64>,
    touch_minor: Option<i64>,
    orientation: Option<i64>,
}

impl SlotState {
    const fn should_emit(&self) -> bool {
        self.active || self.flags.started || self.flags.ended || self.flags.changed
    }

    fn to_json(&self, slot_index: i64) -> Value {
        json!({
            "slot": slot_index,
            "touch_active": self.active,
            "touch_started": self.flags.started,
            "touch_ended": self.flags.ended,
            "touch_sequence_index": self.sequence_index,
            "tracking_id": self.tracking_id,
            "x": self.x,
            "y": self.y,
            "pressure": self.pressure,
            "touch_major": self.touch_major,
            "touch_minor": self.touch_minor,
            "orientation": self.orientation,
        })
    }

    const fn after_emit(&mut self) {
        self.flags.started = false;
        if self.flags.ended {
            self.tracking_id = None;
            self.sequence_index = None;
        }
        self.flags.ended = false;
        self.flags.changed = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::getevent::parser::{ParsedLine, parse_line};
    use crate::getevent::touch_state::TouchState;

    #[test]
    fn touch_state_emits_start_and_end_frames() {
        let lines = [
            "[  1.000001] /dev/input/event4: EV_ABS ABS_MT_TRACKING_ID 00000064",
            "[  1.000001] /dev/input/event4: EV_ABS ABS_MT_POSITION_X 00000090",
            "[  1.000001] /dev/input/event4: EV_ABS ABS_MT_POSITION_Y 00000957",
            "[  1.000001] /dev/input/event4: EV_SYN SYN_REPORT 00000000",
            "[  1.000002] /dev/input/event4: EV_ABS ABS_MT_TRACKING_ID ffffffff",
            "[  1.000002] /dev/input/event4: EV_SYN SYN_REPORT 00000000",
        ];
        let mut state = TouchState::default();
        let mut frames = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let maybe_line_index = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1));
            assert!(maybe_line_index.is_some(), "line index should fit in u64");
            let Some(actual_line_index) = maybe_line_index else {
                continue;
            };
            let parsed = parse_line(line);
            assert!(parsed.is_ok(), "fixture line should parse");
            if let Ok(ParsedLine::InputEvent(event)) = parsed {
                let frame = state.update(&event, actual_line_index);
                assert!(frame.is_ok(), "touch state should update");
                if let Ok(Some(value)) = frame {
                    frames.push(value);
                }
            }
        }

        assert_eq!(frames.len(), 2, "start and end frames should emit");
        assert_eq!(
            frames
                .first()
                .and_then(|frame| frame.pointer("/slots/0/touch_started"))
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "first frame should mark start"
        );
        assert_eq!(
            frames
                .get(1)
                .and_then(|frame| frame.pointer("/slots/0/touch_ended"))
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "second frame should mark end"
        );
    }
}
