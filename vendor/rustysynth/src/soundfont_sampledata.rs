#![allow(dead_code)]

use std::io::Read;

use crate::binary_reader::BinaryReader;
use crate::error::SoundFontError;
use crate::four_cc::FourCC;
use crate::read_counter::ReadCounter;

#[non_exhaustive]
pub(crate) struct SoundFontSampleData {
    pub bits_per_sample: i32,
    pub wave_data: Vec<i16>,
    /// LOCAL PATCH (synththing): the raw `smpl` chunk of a SoundFont 3, whose
    /// samples are compressed (Ogg Vorbis or FLAC). `wave_data` is empty until
    /// `sf3::decompress` fills it from the sample headers.
    pub compressed: Option<Vec<u8>>,
}

impl SoundFontSampleData {
    pub(crate) fn new<R: Read>(reader: &mut R) -> Result<Self, SoundFontError> {
        let chunk_id = BinaryReader::read_four_cc(reader)?;
        if chunk_id != b"LIST" {
            return Err(SoundFontError::ListChunkNotFound);
        }

        let end = BinaryReader::read_u32(reader)? as usize;
        let reader = &mut ReadCounter::new(reader);

        let list_type = BinaryReader::read_four_cc(reader)?;
        if list_type != b"sdta" {
            return Err(SoundFontError::InvalidListChunkType {
                expected: FourCC::from_bytes(*b"sdta"),
                actual: list_type,
            });
        }

        let mut wave_data: Option<Vec<i16>> = None;
        let mut compressed: Option<Vec<u8>> = None;

        while reader.bytes_read() < end {
            let id = BinaryReader::read_four_cc(reader)?;
            let size = BinaryReader::read_u32(reader)? as usize;

            match id.as_bytes() {
                b"smpl" if size >= 4 => {
                    // SoundFont3 compressed sample format: keep the bytes,
                    // the sample headers say where each sample is.
                    let mut magic = [0u8; 4];
                    reader.read_exact(&mut magic)?;
                    let mut chunk = magic.as_slice().chain(&mut *reader);
                    if &magic == b"OggS" || &magic == b"fLaC" {
                        compressed = Some(BinaryReader::read_bytes(&mut chunk, size)?);
                    } else {
                        wave_data = Some(BinaryReader::read_wave_data(&mut chunk, size)?);
                    }
                }
                b"smpl" => BinaryReader::discard_data(reader, size)?,
                b"sm24" => BinaryReader::discard_data(reader, size)?,
                _ => return Err(SoundFontError::ListContainsUnknownId(id)),
            }

            // RIFF pads odd-sized chunks to an even length. Only an SF3's
            // compressed samples can come out odd.
            if size % 2 == 1 && reader.bytes_read() < end {
                BinaryReader::discard_data(reader, 1)?;
            }
        }

        if let Some(compressed) = compressed {
            return Ok(Self {
                bits_per_sample: 16,
                wave_data: Vec::new(),
                compressed: Some(compressed),
            });
        }

        let Some(wave_data) = wave_data else {
            return Err(SoundFontError::SampleDataNotFound);
        };

        Ok(Self {
            bits_per_sample: 16,
            wave_data,
            compressed: None,
        })
    }
}
