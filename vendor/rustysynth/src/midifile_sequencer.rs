#![allow(dead_code)]

use std::cmp;
use std::sync::Arc;

use crate::midifile::Message;
use crate::midifile::MidiFile;
use crate::synthesizer::Synthesizer;

/// An instance of the MIDI file sequencer.
#[derive(Clone, Debug)] // LOCAL PATCH (synththing): Clone, for snapshots
#[non_exhaustive]
pub struct MidiFileSequencer {
    synthesizer: Synthesizer,

    speed: f64,

    midi_file: Option<Arc<MidiFile>>,
    play_loop: bool,

    block_wrote: usize,

    current_time: f64,
    msg_index: usize,
    loop_index: usize,

    // LOCAL PATCH (synththing): bit N set = channel N audible. Note-ons for a
    // muted channel are dropped in `process_events`; everything else still
    // goes through so releases and controller state stay consistent.
    channel_mask: u16,

    // LOCAL PATCH (synththing): score-level held-note velocities, indexed by
    // `channel * 128 + key` (0 = not held). Maintained incrementally in
    // `process_events` so `active_notes` is O(1) instead of re-scanning the
    // message list from the top every frame. Tracks the score regardless of
    // `channel_mask`, so a muted channel's notes still report as active.
    active_grid: Vec<u8>,
}

impl MidiFileSequencer {
    /// Initializes a new instance of the sequencer.
    ///
    /// # Arguments
    ///
    /// * `synthesizer` - The synthesizer to be handled by the sequencer.
    pub fn new(synthesizer: Synthesizer) -> Self {
        Self {
            synthesizer,
            speed: 1.0,
            midi_file: None,
            play_loop: false,
            block_wrote: 0,
            current_time: 0.0,
            msg_index: 0,
            loop_index: 0,
            channel_mask: 0xFFFF,
            active_grid: vec![0u8; 16 * 128],
        }
    }

    /// LOCAL PATCH (synththing): set which channels are audible (bit N set =
    /// channel N plays). Channels newly muted have their ringing voices
    /// released immediately. Not reset by `play`.
    pub fn set_channel_mask(&mut self, mask: u16) {
        let newly_muted = self.channel_mask & !mask;
        self.channel_mask = mask;
        for channel in 0..16 {
            if newly_muted & (1 << channel) != 0 {
                self.synthesizer.note_off_all_channel(channel, false);
            }
        }
    }

    /// Plays the MIDI file.
    ///
    /// # Arguments
    ///
    /// * `midi_file` - The MIDI file to be played.
    /// * `play_loop` - If `true`, the MIDI file loops after reaching the end.
    pub fn play(&mut self, midi_file: &Arc<MidiFile>, play_loop: bool) {
        self.midi_file = Some(Arc::clone(midi_file));
        self.play_loop = play_loop;

        self.block_wrote = self.synthesizer.block_size;

        self.current_time = 0.0;
        self.msg_index = 0;
        self.loop_index = 0;
        self.active_grid.fill(0); // LOCAL PATCH (synththing)

        self.synthesizer.reset()
    }

    /// Stops playing.
    pub fn stop(&mut self) {
        self.midi_file = None;
        self.synthesizer.reset();
    }

    /// Renders the waveform.
    ///
    /// # Arguments
    ///
    /// * `left` - The buffer of the left channel to store the rendered waveform.
    /// * `right` - The buffer of the right channel to store the rendered waveform.
    ///
    /// # Remarks
    ///
    /// The output buffers for the left and right must be the same length.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if left.len() != right.len() {
            panic!("The output buffers for the left and right must be the same length.");
        }

        let left_length = left.len();
        let mut wrote: usize = 0;
        while wrote < left_length {
            if self.block_wrote == self.synthesizer.block_size {
                self.process_events();
                self.block_wrote = 0;
                self.current_time += self.speed * self.synthesizer.block_size as f64
                    / self.synthesizer.sample_rate as f64;
            }

            let src_rem = self.synthesizer.block_size - self.block_wrote;
            let dst_rem = left_length - wrote;
            let rem = cmp::min(src_rem, dst_rem);

            self.synthesizer.render(
                &mut left[wrote..wrote + rem],
                &mut right[wrote..wrote + rem],
            );

            self.block_wrote += rem;
            wrote += rem;
        }
    }

    fn process_events(&mut self) {
        let midi_file = match self.midi_file.as_ref() {
            Some(value) => value,
            None => return,
        };

        while self.msg_index < midi_file.messages.len() {
            let time = midi_file.times[self.msg_index];
            let msg = midi_file.messages[self.msg_index];

            if time <= self.current_time {
                match msg {
                    Message::Normal {
                        status,
                        data1,
                        data2,
                    } => {
                        let channel = status & 0x0F;
                        let command = status & 0xF0;

                        // LOCAL PATCH (synththing): track the score's held notes
                        // regardless of muting, so the visualizer still sees a
                        // muted channel's notes.
                        if command == 0x90 || command == 0x80 {
                            let held = command == 0x90 && data2 > 0;
                            self.active_grid[(channel as usize) * 128 + data1 as usize] =
                                if held { data2 } else { 0 };
                        }

                        // LOCAL PATCH (synththing): swallow note-ons for a muted
                        // channel; let note-offs, CC, program change, etc. pass.
                        let muted = self.channel_mask & (1 << channel) == 0;
                        let note_on = command == 0x90 && data2 > 0;
                        if !(muted && note_on) {
                            self.synthesizer.process_midi_message(
                                channel as i32,
                                command as i32,
                                data1 as i32,
                                data2 as i32,
                            );
                        }
                    }
                    Message::LoopStart if self.play_loop => self.loop_index = self.msg_index,
                    Message::LoopEnd if self.play_loop => {
                        self.current_time = midi_file.times[self.loop_index];
                        self.msg_index = self.loop_index;
                        self.synthesizer.note_off_all(false);
                        self.active_grid.fill(0); // LOCAL PATCH (synththing)
                    }
                    _ => (),
                }
                self.msg_index += 1;
            } else {
                break;
            }
        }

        if self.msg_index == midi_file.messages.len() && self.play_loop {
            self.current_time = midi_file.times[self.loop_index];
            self.msg_index = self.loop_index;
            self.synthesizer.note_off_all(false);
            self.active_grid.fill(0); // LOCAL PATCH (synththing)
        }
    }

    /// Gets the synthesizer handled by the sequencer.
    pub fn get_synthesizer(&self) -> &Synthesizer {
        &self.synthesizer
    }

    /// Gets the currently playing MIDI file.
    pub fn get_midi_file(&self) -> Option<&MidiFile> {
        match &self.midi_file {
            None => None,
            Some(value) => Some(value),
        }
    }

    /// Gets the current playback position in seconds.
    pub fn get_position(&self) -> f64 {
        self.current_time
    }

    /// Gets a value that indicates whether the current playback position is at the end of the sequence.
    ///
    /// # Remarks
    ///
    /// If the `play` method has not yet been called, this value will be `true`.
    /// This value will never be `true` if loop playback is enabled.
    pub fn end_of_sequence(&self) -> bool {
        match &self.midi_file {
            None => true,
            Some(value) => self.msg_index == value.messages.len(),
        }
    }

    /// Gets the current playback speed.
    ///
    /// # Remarks
    ///
    /// The default value is 1.
    /// The tempo will be multiplied by this value during playback.
    pub fn get_speed(&self) -> f64 {
        self.speed
    }

    /// Sets the playback speed.
    ///
    /// # Remarks
    ///
    /// The value must be non-negative.
    pub fn set_speed(&mut self, value: f64) {
        if value < 0.0 {
            panic!("The playback speed must be a non-negative value.");
        }

        self.speed = value;
    }

    // LOCAL PATCH (synththing): note inspection for visualizers. These scan
    // the MIDI message list directly rather than the synthesizer's voices, so
    // they report which keys the *score* has held down right now (and which
    // change soon), not which voices are still ringing out a release tail.

    /// Notes with a note-on and no matching note-off at or before the current
    /// playback position. Empty when no MIDI file is playing.
    pub fn active_notes(&self) -> Vec<ActiveNote> {
        let mut out = Vec::new();
        for channel in 0..16usize {
            for key in 0..128usize {
                let velocity = self.active_grid[channel * 128 + key];
                if velocity > 0 {
                    out.push(ActiveNote {
                        channel: channel as i32,
                        key: key as i32,
                        velocity: velocity as i32,
                    });
                }
            }
        }
        out
    }

    /// Note-on / note-off events occurring within `window` seconds after the
    /// current playback position, earliest first.
    pub fn upcoming_notes(&self, window: f64) -> Vec<NoteEvent> {
        let Some(midi_file) = self.midi_file.as_ref() else {
            return Vec::new();
        };

        let horizon = self.current_time + window.max(0.0);
        let mut events = Vec::new();
        for i in self.msg_index..midi_file.messages.len() {
            let time = midi_file.times[i];
            if time > horizon {
                break;
            }
            if time <= self.current_time {
                continue;
            }
            if let Message::Normal {
                status,
                data1,
                data2,
            } = midi_file.messages[i]
            {
                let command = status & 0xF0;
                if command != 0x90 && command != 0x80 {
                    continue;
                }
                let velocity = data2 as i32;
                let on = command == 0x90 && velocity > 0;
                events.push(NoteEvent {
                    channel: (status & 0x0F) as i32,
                    key: data1 as i32,
                    velocity: if on { velocity } else { 0 },
                    on,
                    time_until: time - self.current_time,
                });
            }
        }

        events
    }
}

/// A note the sequencer's score currently holds down. See
/// [`MidiFileSequencer::active_notes`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ActiveNote {
    pub channel: i32,
    pub key: i32,
    pub velocity: i32,
}

/// An upcoming note-on or note-off. See [`MidiFileSequencer::upcoming_notes`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct NoteEvent {
    pub channel: i32,
    pub key: i32,
    /// Velocity of a note-on; `0` for a note-off.
    pub velocity: i32,
    /// `true` for a note-on, `false` for a note-off.
    pub on: bool,
    /// Seconds from the current playback position until this event.
    pub time_until: f64,
}
