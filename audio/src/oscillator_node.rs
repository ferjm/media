use block::{Chunk, Tick};
use node::{AudioNodeEngine, AudioScheduledSourceNodeMessage, BlockInfo, OnEndedCallback};
use node::{AudioNodeType, ChannelInfo};
use num_traits::cast::NumCast;
use param::{Param, ParamType};

#[derive(Copy, Clone, Debug)]
pub struct PeriodicWaveOptions {
    // XXX https://webaudio.github.io/web-audio-api/#dictdef-periodicwaveoptions
}

#[derive(Copy, Clone, Debug)]
pub enum OscillatorType {
    Sine,
    Square,
    Sawtooth,
    Triangle,
    Custom,
}

#[derive(Copy, Clone, Debug)]
pub struct OscillatorNodeOptions {
    pub oscillator_type: OscillatorType,
    pub freq: f32,
    pub detune: f32,
    pub periodic_wave_options: Option<PeriodicWaveOptions>,
}

impl Default for OscillatorNodeOptions {
    fn default() -> Self {
        OscillatorNodeOptions {
            oscillator_type: OscillatorType::Sine,
            freq: 440.,
            detune: 0.,
            periodic_wave_options: None,
        }
    }
}

#[derive(AudioScheduledSourceNode, AudioNodeCommon)]
pub(crate) struct OscillatorNode {
    channel_info: ChannelInfo,
    frequency: Param,
    detune: Param,
    phase: f64,
    /// Time at which the source should start playing.
    start_at: Option<Tick>,
    /// Time at which the source should stop playing.
    stop_at: Option<Tick>,
    /// The ended event callback.
    onended_callback: Option<OnEndedCallback>,
}

impl OscillatorNode {
    pub fn new(options: OscillatorNodeOptions) -> Self {
        Self {
            channel_info: Default::default(),
            frequency: Param::new(options.freq.into()),
            detune: Param::new(options.detune.into()),
            phase: 0.,
            start_at: None,
            stop_at: None,
            onended_callback: None,
        }
    }

    pub fn update_parameters(&mut self, info: &BlockInfo, tick: Tick) -> bool {
        self.frequency.update(info, tick)
    }
}

impl AudioNodeEngine for OscillatorNode {
    fn node_type(&self) -> AudioNodeType {
        AudioNodeType::OscillatorNode
    }

    fn process(&mut self, mut inputs: Chunk, info: &BlockInfo) -> Chunk {
        // XXX Implement this properly and according to self.options
        // as defined in https://webaudio.github.io/web-audio-api/#oscillatornode

        use std::f64::consts::PI;

        debug_assert!(inputs.len() == 0);

        inputs.blocks.push(Default::default());

        if self.should_play_at(info.frame) == (false, true) {
            self.maybe_trigger_onended_callback();
            return inputs;
        }

        {
            inputs.blocks[0].explicit_silence();
            let mut iter = inputs.blocks[0].iter();

            // Convert all our parameters to the target type for calculations
            let vol: f32 = 1.0;
            let freq = self.frequency.value() as f64;
            let sample_rate = info.sample_rate as f64;
            let two_pi = 2.0 * PI;

            // We're carrying a phase with up to 2pi around instead of working
            // on the sample offset. High sample offsets cause too much inaccuracy when
            // converted to floating point numbers and then iterated over in 1-steps
            //
            // Also, if the frequency changes the phase should not
            let mut step = two_pi * freq / sample_rate;
            while let Some(mut frame) = iter.next() {
                let tick = frame.tick();
                let (should_play_at, should_break) = self.should_play_at(info.frame + tick);
                if !should_play_at {
                    if should_break {
                        self.maybe_trigger_onended_callback();
                        break;
                    }
                    continue;
                }
                if self.update_parameters(info, tick) {
                    step = two_pi * freq / sample_rate;
                }
                let value = vol * f32::sin(NumCast::from(self.phase).unwrap());
                frame.mutate_with(|sample| *sample = value);

                self.phase += step;
                if self.phase >= two_pi {
                    self.phase -= two_pi;
                }
            }
        }
        inputs
    }

    fn input_count(&self) -> u32 {
        0
    }

    fn get_param(&mut self, id: ParamType) -> &mut Param {
        match id {
            ParamType::Frequency => &mut self.frequency,
            ParamType::Detune => &mut self.detune,
            _ => panic!("Unknown param {:?} for OscillatorNode", id),
        }
    }

    make_message_handler!(AudioScheduledSourceNode: handle_source_node_message);
}
