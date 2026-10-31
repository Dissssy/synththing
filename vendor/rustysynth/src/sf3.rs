//! LOCAL PATCH (synththing): SoundFont 3 support. An SF3's `smpl` chunk holds
//! one compressed stream (Ogg Vorbis, or FLAC) per sample instead of 16-bit
//! PCM. Each sample header's start and end are byte offsets of its stream in
//! that chunk, and its loop points count decoded frames from the sample's own
//! start (the layout MuseScore's sftools and Polyphone write, and FluidSynth
//! reads). Decoding every sample at load turns the SoundFont into an ordinary
//! SF2 in memory, so nothing past loading needs to know.

use std::io::{Cursor, ErrorKind};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::SoundFontError;
use crate::sample_header::SampleHeader;

/// Zero frames after each sample, as SF2 requires: the oscillator reads past
/// a sample's end while interpolating.
const PADDING: usize = 46;

/// The sample type bit for a sample that lives in ROM, with no data here.
const ROM: u16 = 0x8000;

/// Decode every sample in `smpl` into one block of 16-bit PCM, pointing each
/// header at its decoded frames. Samples decode in parallel: a big SF3 holds
/// hundreds of megabytes of audio.
pub(crate) fn decompress(
    smpl: &[u8],
    headers: &mut [SampleHeader],
) -> Result<Vec<i16>, SoundFontError> {
    let streams: Vec<&[u8]> = headers
        .iter()
        .map(|header| {
            if header.sample_type & ROM != 0 || header.end <= header.start {
                return &smpl[..0];
            }
            // FluidSynth reads `end` as inclusive; a stray byte past a
            // stream's last page is harmless either way.
            let start = (header.start.max(0) as usize).min(smpl.len());
            let end = (header.end as usize + 1).min(smpl.len());
            &smpl[start..end]
        })
        .collect();

    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per_thread = streams.len().div_ceil(threads).max(1);
    let decoded: Vec<Result<Vec<i16>, String>> = std::thread::scope(|scope| {
        let workers: Vec<_> = streams
            .chunks(per_thread)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|stream| decode(stream))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });

    let mut wave_data = vec![0; PADDING];
    for (sample_id, (header, frames)) in headers.iter_mut().zip(decoded).enumerate() {
        let frames = frames
            .map_err(|reason| SoundFontError::InvalidCompressedSample { sample_id, reason })?;

        let offset = wave_data.len() as i32;
        header.start = offset;
        header.end = offset + frames.len() as i32;
        header.start_loop += offset;
        header.end_loop += offset;

        wave_data.extend(frames);
        wave_data.extend([0; PADDING]);
    }
    Ok(wave_data)
}

/// The first channel of one compressed stream, as 16-bit PCM.
fn decode(stream: &[u8]) -> Result<Vec<i16>, String> {
    if stream.is_empty() {
        return Ok(Vec::new());
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(stream.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            source,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|err| err.to_string())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?;
    let track_id = track.id;
    let length = track.codec_params.n_frames;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|err| err.to_string())?;

    let mut frames = Vec::new();
    let mut buffer: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|err| err.to_string())?;
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        if buffer
            .as_ref()
            .is_none_or(|buffer| buffer.capacity() < decoded.capacity() * channels)
        {
            buffer = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let buffer = buffer.as_mut().unwrap();
        buffer.copy_interleaved_ref(decoded);
        frames.extend(buffer.samples().iter().step_by(channels));
    }
    // The last Vorbis packet decodes past the stream's real end, which only
    // its final page's granule position tells.
    if let Some(length) = length {
        frames.truncate(length as usize);
    }
    Ok(frames)
}
