//! Hands one encoded frame to the screen track without stopping the share loop.
//!
//! The LiveKit handoff runs on this thread. While it is still inside the
//! previous frame, another submit is refused. The caller then keeps reading
//! capture, loss, and the once-a-second log, and it asks for a smaller keyframe.

use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame};
use livekit::webrtc::video_source::{native::NativeVideoSource, VideoResolution};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread;

struct Job {
    payload: Vec<u8>,
    timestamp_us: i64,
    keyframe: bool,
    width: u32,
    height: u32,
}

pub struct TrackSend {
    handoff: Handoff<Job>,
}

impl TrackSend {
    pub fn start(source: NativeVideoSource) -> Self {
        Self {
            handoff: Handoff::start(move |job: Job| {
                let frame = EncodedVideoFrame {
                    codec: EncodedVideoCodec::H264,
                    payload: &job.payload,
                    timestamp_us: job.timestamp_us,
                    frame_type: if job.keyframe {
                        EncodedFrameType::Key
                    } else {
                        EncodedFrameType::Delta
                    },
                    resolution: VideoResolution {
                        width: job.width,
                        height: job.height,
                    },
                    frame_metadata: None,
                };
                let _ = source.capture_encoded_frame(&frame);
            }),
        }
    }

    /// `true` when this frame was accepted. `false` when the previous frame
    /// is still inside the handoff.
    pub fn try_submit(
        &self,
        payload: Vec<u8>,
        timestamp_us: i64,
        keyframe: bool,
        width: u32,
        height: u32,
    ) -> bool {
        self.handoff.try_submit(Job {
            payload,
            timestamp_us,
            keyframe,
            width,
            height,
        })
    }
}

struct Handoff<T> {
    tx: SyncSender<T>,
}

impl<T: Send + 'static> Handoff<T> {
    fn start(mut work: impl FnMut(T) + Send + 'static) -> Self {
        let (tx, rx) = sync_channel(0);
        let spawned = thread::Builder::new()
            .name("screen-track".to_string())
            .spawn(move || deliver(rx, &mut work));
        if spawned.is_err() {
            // `rx` is dropped with the closure, so submits fail and the caller
            // keeps the share loop alive.
        }
        Self { tx }
    }

    fn try_submit(&self, job: T) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn deliver<T>(rx: Receiver<T>, work: &mut impl FnMut(T)) {
    while let Ok(job) = rx.recv() {
        work(job);
    }
}

#[cfg(test)]
mod tests {
    use super::Handoff;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn a_busy_handoff_refuses_the_next_frame_and_then_accepts() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handoff = Handoff::start(move |value: u32| {
            if value == 1 {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
            }
        });
        let started = Instant::now();
        while !handoff.try_submit(1) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "the handoff thread did not take the first frame"
            );
            thread::sleep(Duration::from_millis(10));
        }
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the first frame should be inside the handoff");
        assert!(!handoff.try_submit(2));
        release_tx.send(()).expect("release");
        let started = Instant::now();
        while !handoff.try_submit(3) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "the handoff should accept a frame once the previous one returns"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
