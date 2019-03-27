use byte_slice_cast::*;
use gst;
use gst::prelude::*;
use gst_app;
use servo_media_streams::{AutoMediaStream, MediaStreamCallbacks};
use std::sync::Arc;

pub struct GStreamerMediaStreamProgress(gst::buffer::MappedBuffer<gst::buffer::Readable>);

impl AsRef<[u8]> for GStreamerMediaStreamProgress {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().as_slice_of::<u8>().unwrap()
    }
}

pub struct GStreamerAutoMediaStream {}

impl AutoMediaStream for GStreamerAutoMediaStream {
    fn play(callbacks: MediaStreamCallbacks) {
        let pipeline = gst::Pipeline::new(None);
        let callbacks = Arc::new(callbacks);

        let src = gst::ElementFactory::make("videotestsrc", None).unwrap();
        let sink = gst::ElementFactory::make("appsink", None).unwrap();

        pipeline.add_many(&[&src, &sink]).unwrap();
        gst::Element::link_many(&[&src, &sink]).unwrap();

        let appsink = sink.clone().dynamic_cast::<gst_app::AppSink>().unwrap();
        let callbacks_ = callbacks.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::new()
                .new_sample(move |appsink| {
                    let sample =
                        appsink.pull_sample().ok_or(gst::FlowError::Eos)?;
                    let buffer = sample.get_buffer().ok_or_else(|| {
                        callbacks_.error();
                        gst::FlowError::Error
                    })?;

                    let buffer = buffer.clone();
                    let map = if let Ok(map) =
                        buffer.into_mapped_buffer_readable()
                    {
                        map
                    } else {
                        callbacks_.error();
                        return Err(gst::FlowError::Error);
                    };
                    let progress = Box::new(GStreamerMediaStreamProgress(map));
                    callbacks_.progress(progress);

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline.set_state(gst::State::Playing).unwrap();
        // XXX probably need to wait
    }
}
