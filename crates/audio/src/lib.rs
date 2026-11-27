//! Prepare a looping synth buffer on the control thread. The device callback only
//! copies immutable samples and publishes its position at buffer boundaries.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use shared::{Project, Track};
use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};

type AudioError = Box<dyn Error + Send + Sync>;

pub fn render(project: &Project, tracks: &[Track], rate: u32) -> Result<Vec<[f32; 2]>, AudioError> {
    project.validate()?;
    if tracks.len() != project.tracks.len() || rate == 0 {
        return Err("missing tracks or invalid sample rate".into());
    }
    let beat = f64::from(rate) * 60.0 / f64::from(project.bpm);
    let bar = f64::from(project.time_signature[0]) * 4.0 / f64::from(project.time_signature[1]);
    let mut beats = project
        .sections
        .iter()
        .map(|s| (f64::from(s.start_bar) + f64::from(s.bars)) * bar)
        .fold(1.0, f64::max);
    for (track, id) in tracks.iter().zip(&project.tracks) {
        track.validate(id)?;
        for p in &track.patterns {
            for n in &p.notes {
                beats = beats.max(p.start_beat + n.start_beat + n.duration_beats);
            }
        }
    }
    let frames = (beats * beat).ceil();
    if frames > 16_000_000.0 {
        return Err("song exceeds the in-memory renderer limit".into());
    }
    let mut pcm = vec![[0.0f32; 2]; frames as usize];
    let solo = tracks.iter().any(|t| t.mixer.solo);
    for t in tracks
        .iter()
        .filter(|t| !t.mixer.mute && (!solo || t.mixer.solo))
    {
        for p in &t.patterns {
            for n in &p.notes {
                let begin = ((p.start_beat + n.start_beat) * beat) as usize;
                let length = (n.duration_beats * beat).ceil() as usize;
                let hz = 440.0 * 2f64.powf((f64::from(n.pitch) - 69.0) / 12.0);
                let step = std::f64::consts::TAU * hz / f64::from(rate);
                let gain = t.mixer.volume * f32::from(n.velocity) / 127.0 * 0.24;
                for (i, frame) in pcm.iter_mut().skip(begin).take(length).enumerate() {
                    let fade = (i.min(length - i) as f32 / (rate as f32 * 0.005)).min(1.0);
                    let sample = (step * i as f64).sin() as f32 * gain * fade;
                    frame[0] += sample * (1.0 - t.mixer.pan.max(0.0));
                    frame[1] += sample * (1.0 + t.mixer.pan.min(0.0));
                }
            }
        }
    }
    Ok(pcm)
}

/// Device-independent PCM shared between successive transport starts.
pub struct Clip(Arc<[[f32; 2]]>);

impl Clip {
    const RATE: u32 = 48_000;

    pub fn prepare(project: &Project, tracks: &[Track]) -> Result<Self, AudioError> {
        Ok(Self(render(project, tracks, Self::RATE)?.into()))
    }
}

#[derive(Default)]
struct StreamFailure(AtomicU8);

impl StreamFailure {
    // CPAL can invoke this on the render thread. Only publish an error code.
    fn signal(&self, error: &cpal::StreamError) {
        let code = match error {
            cpal::StreamError::DeviceNotAvailable => 1,
            cpal::StreamError::BackendSpecific { .. } => 2,
        };
        self.0.store(code, Ordering::Relaxed);
    }

    fn message(&self) -> Option<&'static str> {
        match self.0.load(Ordering::Relaxed) {
            1 => Some("audio output device is no longer available"),
            2 => Some("audio output stream reported a backend failure"),
            _ => None,
        }
    }
}

pub struct Playback {
    _stream: cpal::Stream,
    failure: Arc<StreamFailure>,
    position: Arc<AtomicUsize>,
}

impl Playback {
    pub fn failure(&self) -> Option<&'static str> {
        self.failure.message()
    }

    pub fn position(&self) -> usize {
        self.position.load(Ordering::Relaxed)
    }

    pub fn start(clip: &Clip, offset: usize) -> Result<Self, AudioError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no output device")?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let pcm = clip.0.clone();
        let position = Arc::new(AtomicUsize::new(offset % pcm.len()));
        let failure = Arc::<StreamFailure>::default();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>,
            cpal::SampleFormat::I16 => stream::<i16>,
            cpal::SampleFormat::U16 => stream::<u16>,
            _ => return Err("unsupported output sample format".into()),
        }(&device, &config, pcm, position.clone(), failure.clone())?;
        stream.play()?;
        Ok(Self {
            _stream: stream,
            failure,
            position,
        })
    }
}

fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    pcm: Arc<[[f32; 2]]>,
    position: Arc<AtomicUsize>,
    failure: Arc<StreamFailure>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let mut cursor = position.load(Ordering::Relaxed) as f64;
    let step = f64::from(Clip::RATE) / f64::from(config.sample_rate.0);
    let channels = usize::from(config.channels);
    device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            for frame in out.chunks_mut(channels) {
                let i = cursor as usize;
                let [l, r] = pcm[i];
                let [next_l, next_r] = pcm[(i + 1) % pcm.len()];
                let fraction = (cursor - i as f64) as f32;
                let (l, r) = (l + fraction * (next_l - l), r + fraction * (next_r - r));
                for (channel, sample) in frame.iter_mut().enumerate() {
                    let value = if channels == 1 {
                        (l + r) * 0.5
                    } else {
                        match channel {
                            0 => l,
                            1 => r,
                            _ => 0.0,
                        }
                    };
                    *sample = T::from_sample(value.clamp(-1.0, 1.0));
                }
                cursor = (cursor + step) % pcm.len() as f64;
            }
            position.store(cursor as usize, Ordering::Relaxed);
        },
        move |error| failure.signal(&error),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_thread_reads_stream_failure_after_callback_returns() {
        let failure = Arc::<StreamFailure>::default();
        assert_eq!(failure.message(), None);
        std::thread::scope(|scope| {
            scope.spawn(|| failure.signal(&cpal::StreamError::DeviceNotAvailable));
        });
        assert_eq!(
            failure.message(),
            Some("audio output device is no longer available")
        );
        failure.signal(&cpal::StreamError::BackendSpecific {
            err: cpal::BackendSpecificError {
                description: "test".into(),
            },
        });
        assert_eq!(
            failure.message(),
            Some("audio output stream reported a backend failure")
        );
    }
}
