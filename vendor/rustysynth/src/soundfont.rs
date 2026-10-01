#![allow(dead_code)]

use std::io::Read;

use crate::binary_reader::BinaryReader;
use crate::error::SoundFontError;
use crate::four_cc::FourCC;
use crate::instrument::Instrument;
use crate::preset::Preset;
use crate::sample_header::SampleHeader;
use crate::soundfont_info::SoundFontInfo;
use crate::soundfont_parameters::SoundFontParameters;
use crate::soundfont_sampledata::SoundFontSampleData;
use crate::generator_type::GeneratorType;
use crate::LoopMode;

/// Reperesents a SoundFont.
#[derive(Debug)]
#[non_exhaustive]
pub struct SoundFont {
    pub(crate) info: SoundFontInfo,
    pub(crate) bits_per_sample: i32,
    pub(crate) wave_data: Vec<i16>,
    pub(crate) sample_headers: Vec<SampleHeader>,
    pub(crate) presets: Vec<Preset>,
    pub(crate) instruments: Vec<Instrument>,
}

impl SoundFont {
    /// Loads a SoundFont from the stream.
    ///
    /// # Arguments
    ///
    /// * `reader` - The data stream used to load the SoundFont.
    pub fn new<R: Read>(reader: &mut R) -> Result<Self, SoundFontError> {
        let chunk_id = BinaryReader::read_four_cc(reader)?;
        if chunk_id != b"RIFF" {
            return Err(SoundFontError::RiffChunkNotFound);
        }

        let _size = BinaryReader::read_i32(reader);

        let form_type = BinaryReader::read_four_cc(reader)?;
        if form_type != b"sfbk" {
            return Err(SoundFontError::InvalidRiffChunkType {
                expected: FourCC::from_bytes(*b"sfbk"),
                actual: form_type,
            });
        }

        let info = SoundFontInfo::new(reader)?;
        let sample_data = SoundFontSampleData::new(reader)?;
        let parameters = SoundFontParameters::new(reader)?;

        let mut sound_font = Self {
            info,
            bits_per_sample: sample_data.bits_per_sample,
            wave_data: sample_data.wave_data,
            sample_headers: parameters.sample_headers,
            presets: parameters.presets,
            instruments: parameters.instruments,
        };

        // LOCAL PATCH (synththing): upstream rejects the whole SoundFont if any
        // instrument region has out-of-range sample/loop points (see rustysynth
        // issues #22, #33, PR #51). Plenty of real-world soundfonts, e.g. the
        // "Ultimate Earthbound" font, trip this even though FluidSynth plays
        // them fine. Instead of failing, clamp the offending regions to a safe,
        // self-consistent range so the oscillator can never index out of bounds.
        sound_font.repair_regions();

        Ok(sound_font)
    }

    fn repair_regions(&mut self) {
        let data_len = self.wave_data.len();
        if data_len < 2 {
            return;
        }
        let max_index = (data_len - 1) as i32;

        for instrument in &mut self.instruments {
            for region in &mut instrument.regions {
                // Effective bounds, including the per-region address-offset generators.
                let start = region.get_sample_start();
                let end = region.get_sample_end();
                let start_loop = region.get_sample_start_loop();
                let end_loop = region.get_sample_end_loop();
                let looping = region.get_sample_modes() != LoopMode::NoLoop;

                let broken = start < 0
                    || start_loop < 0
                    || end >= data_len as i32
                    || end_loop >= data_len as i32
                    || end <= start
                    || end_loop < start_loop
                    || (looping && start_loop >= end_loop);
                if !broken {
                    continue;
                }

                // Clamp into a range that satisfies every invariant the
                // oscillator relies on: 0 <= start < end <= max_index and
                // 0 <= start_loop < end_loop <= max_index.
                let start = start.clamp(0, max_index - 1);
                let end = end.clamp(start + 1, max_index);
                let start_loop = start_loop.clamp(0, max_index - 1);
                let end_loop = end_loop.clamp(start_loop + 1, max_index);

                region.sample_start = start;
                region.sample_end = end;
                region.sample_start_loop = start_loop;
                region.sample_end_loop = end_loop;

                // Zero the address-offset generators so the getters now return
                // exactly the clamped raw values above.
                for generator in [
                    GeneratorType::START_ADDRESS_OFFSET,
                    GeneratorType::END_ADDRESS_OFFSET,
                    GeneratorType::START_LOOP_ADDRESS_OFFSET,
                    GeneratorType::END_LOOP_ADDRESS_OFFSET,
                    GeneratorType::START_ADDRESS_COARSE_OFFSET,
                    GeneratorType::END_ADDRESS_COARSE_OFFSET,
                    GeneratorType::START_LOOP_ADDRESS_COARSE_OFFSET,
                    GeneratorType::END_LOOP_ADDRESS_COARSE_OFFSET,
                ] {
                    region.gs[generator as usize] = 0;
                }
            }
        }
    }

    /// Gets the information of the SoundFont.
    pub fn get_info(&self) -> &SoundFontInfo {
        &self.info
    }

    /// Gets the bits per sample of the sample data.
    pub fn get_bits_per_sample(&self) -> i32 {
        self.bits_per_sample
    }

    /// Gets the sample data.
    pub fn get_wave_data(&self) -> &[i16] {
        &self.wave_data[..]
    }

    /// Gets the samples of the SoundFont.
    pub fn get_sample_headers(&self) -> &[SampleHeader] {
        &self.sample_headers[..]
    }

    /// Gets the presets of the SoundFont.
    pub fn get_presets(&self) -> &[Preset] {
        &self.presets[..]
    }

    /// Gets the instruments of the SoundFont.
    pub fn get_instruments(&self) -> &[Instrument] {
        &self.instruments[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, path::PathBuf};

    fn samples_dir_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("samples")
    }

    #[test]
    fn test_load_reject_sf3() {
        let path = samples_dir_path().join("dummy.sf3");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::UnsupportedSampleFormat)
        ));
    }

    // smpl sub-chunk exists, but is zero-length.
    #[test]
    fn test_load_empty_samples() {
        let path = samples_dir_path().join("test_empty_samples.sf2");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::SampleDataNotFound)
        ));
    }
}
