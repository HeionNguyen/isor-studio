use serde::{Deserialize, Serialize};

/// Metadata in project.yaml. Each track ID resolves to tracks/<id>.yaml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u8,
    pub title: String,
    pub bpm: u16,
    pub time_signature: [u8; 2],
    pub key: String,
    pub sections: Vec<Section>,
    pub tracks: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub start_bar: u32,
    pub bars: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub instrument: InstrumentSpec,
    pub mixer: MixerState,
    pub patterns: Vec<Pattern>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentSpec {
    BasicSynth,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixerState {
    pub volume: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub start_beat: f64,
    pub notes: Vec<NoteEvent>,
}
/// Start is relative to the pattern, in quarter notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteEvent {
    pub start_beat: f64,
    pub duration_beats: f64,
    pub pitch: u8,
    pub velocity: u8,
}

impl Project {
    pub fn validate(&self) -> Result<(), &'static str> {
        let [beats, unit] = self.time_signature;
        if self.version != 0 {
            return Err("unsupported project version");
        }
        if self.bpm == 0 || beats == 0 || !unit.is_power_of_two() {
            return Err("invalid tempo or time signature");
        }
        let mut ids = std::collections::HashSet::new();
        for id in &self.tracks {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !ids.insert(id)
            {
                return Err("invalid or duplicate track ID");
            }
        }
        if self.sections.iter().any(|s| s.bars == 0) {
            return Err("section must span at least one bar");
        }
        Ok(())
    }
}
impl Track {
    pub fn validate(&self, expected_id: &str) -> Result<(), &'static str> {
        if self.id != expected_id {
            return Err("track ID differs from project reference");
        }
        if !(0.0..=1.0).contains(&self.mixer.volume) || !(-1.0..=1.0).contains(&self.mixer.pan) {
            return Err("invalid mixer gain or pan");
        }
        for p in &self.patterns {
            if !p.start_beat.is_finite() || p.start_beat < 0.0 {
                return Err("invalid pattern start");
            }
            for n in &p.notes {
                if n.pitch > 127 || n.velocity > 127 {
                    return Err("invalid MIDI pitch or velocity");
                }
                if !n.start_beat.is_finite()
                    || n.start_beat < 0.0
                    || !n.duration_beats.is_finite()
                    || n.duration_beats <= 0.0
                {
                    return Err("invalid note timing");
                }
            }
        }
        Ok(())
    }
}
