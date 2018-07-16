use block::Tick;
use node::BlockInfo;

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub enum ParamType {
    Frequency,
    Detune,
    Gain,
    PlaybackRate,
}

/// An AudioParam.
///
/// https://webaudio.github.io/web-audio-api/#AudioParam
#[derive(Debug)]
pub struct Param {
    val: f32,
    kind: ParamRate,
    events: Vec<AutomationEvent>,
    current_event: usize,
    event_start_time: Tick,
    event_start_value: f32,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ParamRate {
    /// Value is held for entire block
    KRate,
    /// Value is updated each frame
    ARate,
}

impl Param {
    pub fn new(val: f32) -> Self {
        Param {
            val,
            kind: ParamRate::ARate,
            events: vec![],
            current_event: 0,
            event_start_time: Tick(0),
            event_start_value: val,
        }
    }

    /// Update the value of this param to the next
    ///
    /// Returns true if anything changed
    pub fn update(&mut self, block: &BlockInfo, tick: Tick) -> bool {
        if tick.0 != 0 && self.kind == ParamRate::KRate {
            return false;
        }

        if self.events.len() <= self.current_event {
            return false;
        }

        let current_tick = block.absolute_tick(tick);
        let mut current_event = &self.events[self.current_event];

        // move to next event if necessary
        // XXXManishearth k-rate events may get skipped over completely by this
        // method. Firefox currently doesn't support these, however, so we can
        // handle those later
        loop {
            let mut move_next = false;
            if let Some(done_time) = current_event.done_time() {
                // If this event is done, move on
                if done_time < current_tick {
                    move_next = true;
                }
            } else if let Some(next) = self.events.get(self.current_event + 1) {
                // this event has no done time and we must run it till the next one
                // starts
                if let Some(start_time) = next.start_time() {
                    // if the next one is ready to start, move on
                    if start_time <= current_tick {
                        move_next = true;
                    }
                } else {
                    // If we have a next event with no start time and
                    // the current event has no done time, this *has* to be because
                    // the current event is SetTargetAtTime and the next is a Ramp
                    // event. In this case we skip directly to the ramp assuming
                    // the SetTarget is ready to start (or has started already)
                    if current_event.time() <= current_tick {
                        move_next = true;
                    } else {
                        // This is a SetTarget event before its start time, ignore
                        return false;
                    }
                }
            }
            if move_next {
                self.current_event += 1;
                self.event_start_value = self.val;
                self.event_start_time = current_tick;
                if let Some(next) = self.events.get(self.current_event + 1) {
                    current_event = next;
                    // may need to move multiple times
                    continue;
                } else {
                    return false;
                }
            }
            break;
        }

        current_event.run(
            &mut self.val,
            current_tick,
            self.event_start_time,
            self.event_start_value,
        )
    }

    pub fn value(&self) -> f32 {
        self.val
    }

    pub fn set_rate(&mut self, rate: ParamRate) {
        self.kind = rate;
    }

    pub(crate) fn insert_event(&mut self, event: AutomationEvent) {
        if let AutomationEvent::SetValue(val) = event {
            self.val = val;
            self.event_start_value = val;
            return;
        }

        let time = event.time();

        let result = self.events.binary_search_by(|e| e.time().cmp(&time));
        // XXXManishearth this should handle overlapping events
        let idx = match result {
            Ok(idx) => idx,
            Err(idx) => idx,
        };

        // XXXManishearth this isn't quite correct, this
        // doesn't handle cases for when this lands inside a running
        // event
        if let Some(is_hold) = event.cancel_event() {
            self.events.truncate(idx);
            if !is_hold {
                // If we cancelled the current event, reset
                // the value to what it was before
                if self.current_event >= self.events.len() {
                    self.val = self.event_start_value;
                }
                // don't actually insert the event
                return;
            }
        }
        self.events.insert(idx, event);
        // XXXManishearth handle inserting events with a time before that
        // of the current one
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum RampKind {
    Linear,
    Exponential,
}

#[derive(Clone, Copy, PartialEq, Debug)]
/// https://webaudio.github.io/web-audio-api/#dfn-automation-event
pub(crate) enum AutomationEvent {
    SetValue(f32),
    SetValueAtTime(f32, Tick),
    RampToValueAtTime(RampKind, f32, Tick),
    SetTargetAtTime(f32, Tick, /* time constant, units of Tick */ f64),
    CancelAndHoldAtTime(Tick),
    CancelScheduledValues(Tick),
}

#[derive(Clone, Copy, PartialEq, Debug)]
/// An AutomationEvent that uses times in s instead of Ticks
pub enum UserAutomationEvent {
    SetValue(f32),
    SetValueAtTime(f32, /* time */ f64),
    RampToValueAtTime(RampKind, f32, /* time */ f64),
    SetTargetAtTime(f32, f64, /* time constant, units of s */ f64),
    // SetValueCurveAtTime(Vec<f32>, Tick, /* duration */ Tick)
    CancelAndHoldAtTime(f64),
    CancelScheduledValues(f64),
}

impl UserAutomationEvent {
    pub(crate) fn to_event(self, rate: f32) -> AutomationEvent {
        match self {
            UserAutomationEvent::SetValue(val) => AutomationEvent::SetValue(val),
            UserAutomationEvent::SetValueAtTime(val, time) => {
                AutomationEvent::SetValueAtTime(val, Tick::from_time(time, rate))
            }
            UserAutomationEvent::RampToValueAtTime(kind, val, time) => {
                AutomationEvent::RampToValueAtTime(kind, val, Tick::from_time(time, rate))
            }
            UserAutomationEvent::SetTargetAtTime(val, start, tau) => {
                AutomationEvent::SetTargetAtTime(
                    val,
                    Tick::from_time(start, rate),
                    tau * rate as f64,
                )
            }
            UserAutomationEvent::CancelScheduledValues(t) => {
                AutomationEvent::CancelScheduledValues(Tick::from_time(t, rate))
            }
            UserAutomationEvent::CancelAndHoldAtTime(t) => {
                AutomationEvent::CancelAndHoldAtTime(Tick::from_time(t, rate))
            }
        }
    }
}

impl AutomationEvent {
    /// The time of the event used for ordering
    pub fn time(&self) -> Tick {
        match *self {
            AutomationEvent::SetValueAtTime(_, tick) => tick,
            AutomationEvent::RampToValueAtTime(_, _, tick) => tick,
            AutomationEvent::SetTargetAtTime(_, start, _) => start,
            AutomationEvent::CancelAndHoldAtTime(t) => t,
            AutomationEvent::CancelScheduledValues(..) | AutomationEvent::SetValue(..) => {
                unreachable!("CancelScheduledValues/SetValue should never appear in the timeline")
            }
        }
    }

    pub fn done_time(&self) -> Option<Tick> {
        match *self {
            AutomationEvent::SetValueAtTime(_, tick) => Some(tick),
            AutomationEvent::RampToValueAtTime(_, _, tick) => Some(tick),
            AutomationEvent::SetTargetAtTime(..) => None,
            AutomationEvent::CancelAndHoldAtTime(t) => Some(t),
            AutomationEvent::CancelScheduledValues(..) | AutomationEvent::SetValue(..) => {
                unreachable!("CancelScheduledValues/SetValue should never appear in the timeline")
            }
        }
    }

    pub fn start_time(&self) -> Option<Tick> {
        match *self {
            AutomationEvent::SetValueAtTime(_, tick) => Some(tick),
            AutomationEvent::RampToValueAtTime(..) => None,
            AutomationEvent::SetTargetAtTime(_, start, _) => Some(start),
            AutomationEvent::CancelAndHoldAtTime(t) => Some(t),
            AutomationEvent::CancelScheduledValues(..) | AutomationEvent::SetValue(..) => {
                unreachable!("CancelScheduledValues/SetValue should never appear in the timeline")
            }
        }
    }

    /// Returns Some if it's a cancel event
    /// the boolean is if it's CancelAndHold
    pub fn cancel_event(&self) -> Option<bool> {
        match *self {
            AutomationEvent::CancelAndHoldAtTime(..) => Some(true),
            AutomationEvent::CancelScheduledValues(..) => Some(false),
            _ => None,
        }
    }

    /// Update a parameter based on this event
    ///
    /// Returns true if something changed
    pub fn run(
        &self,
        value: &mut f32,
        current_tick: Tick,
        event_start_time: Tick,
        event_start_value: f32,
    ) -> bool {
        if let Some(start_time) = self.start_time() {
            if start_time > current_tick {
                // The previous event finished and we advanced to this
                // event, but it's not started yet. Return early
                return false;
            }
        }

        match *self {
            AutomationEvent::SetValueAtTime(val, time) => {
                if current_tick == time {
                    *value = val;
                    true
                } else {
                    false
                }
            }
            AutomationEvent::RampToValueAtTime(kind, val, time) => {
                let progress =
                    (current_tick - event_start_time).0 as f32 / (time - event_start_time).0 as f32;
                match kind {
                    RampKind::Linear => {
                        *value = event_start_value + (val - event_start_value) * progress;
                    }
                    RampKind::Exponential => {
                        *value = event_start_value * (val / event_start_value).powf(progress);
                    }
                }
                true
            }
            AutomationEvent::SetTargetAtTime(val, start, tau) => {
                let exp = -((current_tick - start) / tau);
                *value = val + (event_start_value - val) * exp.exp() as f32;
                true
            }
            AutomationEvent::CancelAndHoldAtTime(..) => false,
            AutomationEvent::CancelScheduledValues(..) | AutomationEvent::SetValue(..) => {
                unreachable!("CancelScheduledValues/SetValue should never appear in the timeline")
            }
        }
    }
}
