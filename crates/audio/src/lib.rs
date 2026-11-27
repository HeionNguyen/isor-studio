//! Prepare a looping synth buffer on the control thread. The device callback only
//! copies immutable samples and publishes its position at buffer boundaries.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use shared::{Project, Track};
use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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

pub struct Playback {
    _stream: cpal::Stream,
    position: Arc<AtomicUsize>,
}

impl Playback {
    pub fn position(&self) -> usize {
        self.position.load(Ordering::Relaxed)
    }

    pub fn start(project: &Project, tracks: &[Track], offset: usize) -> Result<Self, AudioError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no output device")?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let pcm = render(project, tracks, config.sample_rate.0)?;
        let position = Arc::new(AtomicUsize::new(offset % pcm.len()));
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>,
            cpal::SampleFormat::I16 => stream::<i16>,
            cpal::SampleFormat::U16 => stream::<u16>,
            _ => return Err("unsupported output sample format".into()),
        }(&device, &config, pcm, position.clone())?;
        stream.play()?;
        Ok(Self {
            _stream: stream,
            position,
        })
    }
}

fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    pcm: Vec<[f32; 2]>,
    position: Arc<AtomicUsize>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let mut cursor = position.load(Ordering::Relaxed);
    let channels = usize::from(config.channels);
    device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            for frame in out.chunks_mut(channels) {
                let [l, r] = pcm[cursor];
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
                cursor = (cursor + 1) % pcm.len();
            }
            position.store(cursor, Ordering::Relaxed);
        },
        |err| eprintln!("audio device error: {err}"),
        None,
    )
}
