use crate::media_stream::{GStreamerMediaStream, StreamType};
use gst;
use gst::prelude::*;
use std::i32;

pub enum Constrain<T> {
    Value(T),
    Range(ConstrainRange<T>),
}

impl<'a> Constrain<u64> {
    fn add_to_caps(
        &self,
        name: &str,
        min: u64,
        max: u64,
        builder: gst::caps::Builder<'a>,
    ) -> Option<gst::caps::Builder<'a>> {
        match self {
            Constrain::Value(v) => Some(builder.field(name, v)),
            Constrain::Range(r) => {
                let min = into_i32(r.min.unwrap_or(min));
                let max = into_i32(r.max.unwrap_or(max));
                let range = gst::IntRange::<i32>::new(min, max);
                if let Some(ideal) = r.ideal {
                    let ideal = into_i32(ideal);
                    let array = gst::List::new(&[&ideal, &range]);
                    Some(builder.field(name, &array))
                } else {
                    Some(builder.field(name, &range))
                }
            }
        }
    }
}

fn into_i32(x: u64) -> i32 {
    if x > i32::MAX as u64 {
        i32::MAX
    } else {
        x as i64 as i32
    }
}

impl<'a> Constrain<f64> {
    fn add_to_caps(
        &self,
        name: &str,
        min: i32,
        max: i32,
        builder: gst::caps::Builder<'a>,
    ) -> Option<gst::caps::Builder<'a>> {
        match self {
            Constrain::Value(v) => {
                Some(builder.field("name", &gst::Fraction::approximate_f64(*v)?))
            }
            Constrain::Range(r) => {
                let min = r
                    .min
                    .and_then(|v| gst::Fraction::approximate_f64(v))
                    .unwrap_or(gst::Fraction::new(min, 1));
                let max = r
                    .max
                    .and_then(|v| gst::Fraction::approximate_f64(v))
                    .unwrap_or(gst::Fraction::new(max, 1));
                let range = gst::FractionRange::new(min, max);
                if let Some(ideal) = r.ideal.and_then(|v| gst::Fraction::approximate_f64(v)) {
                    let array = gst::List::new(&[&ideal, &range]);
                    Some(builder.field(name, &array))
                } else {
                    Some(builder.field(name, &range))
                }
            }
        }
    }
}

pub struct ConstrainRange<T> {
    min: Option<T>,
    max: Option<T>,
    ideal: Option<T>,
}

pub enum ConstrainBool {
    Ideal(bool),
    Exact(bool),
}

#[derive(Default)]
pub struct MediaTrackConstraintSet {
    width: Option<Constrain<u64>>,
    height: Option<Constrain<u64>>,
    aspect: Option<Constrain<f64>>,
    frame_rate: Option<Constrain<f64>>,
    sample_rate: Option<Constrain<f64>>,
}

// TODO(Manishearth): Should support a set of constraints
impl MediaTrackConstraintSet {
    fn into_caps(self, format: &str) -> Option<gst::Caps> {
        let mut builder = gst::Caps::builder(format);
        if let Some(w) = self.width {
            builder = w.add_to_caps("width", 0, 1000000, builder)?;
        }
        if let Some(h) = self.height {
            builder = h.add_to_caps("height", 0, 1000000, builder)?;
        }
        if let Some(aspect) = self.aspect {
            builder = aspect.add_to_caps("pixel-aspect-ratio", 0, 1000000, builder)?;
        }
        if let Some(fr) = self.frame_rate {
            builder = fr.add_to_caps("framerate", 0, 1000000, builder)?;
        }
        if let Some(sr) = self.sample_rate {
            builder = sr.add_to_caps("rate", 0, 1000000, builder)?;
        }
        Some(builder.build())
    }
}

struct GstMediaDevices {
    monitor: gst::DeviceMonitor,
}

impl GstMediaDevices {
    pub fn new() -> Self {
        Self {
            monitor: gst::DeviceMonitor::new(),
        }
    }

    pub fn get_track(
        &self,
        video: bool,
        constraints: MediaTrackConstraintSet,
    ) -> Option<GstMediaTrack> {
        let (format, filter) = if video {
            ("video/x-raw", "Video/Source")
        } else {
            ("audio/x-raw", "Audio/Source")
        };
        let caps = constraints.into_caps(format)?;
        println!("requesting {:?}", caps);
        let f = self.monitor.add_filter(filter, &caps);
        let devices = self.monitor.get_devices();
        if f.is_some() {
            let _ = self.monitor.remove_filter(f.unwrap());
        }
        if let Some(d) = devices.get(0) {
            println!("{:?}", d.get_caps());
            let element = d.create_element(None)?;
            Some(GstMediaTrack { element })
        } else {
            None
        }
    }
}

pub struct GstMediaTrack {
    element: gst::Element,
}

fn create_input_stream(stream_type: StreamType) -> Option<GStreamerMediaStream> {
    let devices = GstMediaDevices::new();
    let constraints = MediaTrackConstraintSet::default();
    devices
        .get_track(stream_type == StreamType::Video, constraints)
        .map(|track| {
            let f = match stream_type {
                StreamType::Audio => GStreamerMediaStream::create_audio_from,
                StreamType::Video => GStreamerMediaStream::create_video_from,
            };
            f(track.element)
        })
}

pub fn create_audioinput_stream() -> Option<GStreamerMediaStream> {
    create_input_stream(StreamType::Audio)
}

pub fn create_videoinput_stream() -> Option<GStreamerMediaStream> {
    create_input_stream(StreamType::Video)
}
