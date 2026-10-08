// SPDX-License-Identifier: LGPL-2.1-or-later
// Port of FFmpeg 2da55bf libavformat/mov.c: the "old uncompressed audio
// chunk demuxing" of mov_build_index, with the frame sizes that
// mov_parse_stsd_audio, mov_read_stsz and mov_finalize_stsd_codec set, and
// mov_read_packet's packet durations.
// Copyright (c) 2001 Fabrice Bellard; Copyright (c) 2009 Baptiste Coudurier
// (mov.c).
//
// This file is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the Free
// Software Foundation; either version 2.1, or (at your option) any later version.
// It is distributed WITHOUT ANY WARRANTY; without even the implied warranty
// of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See LICENSE-LGPL.

//! QuickTime sound tables count one entry per sample (`stts` with a single
//! duration of 1) for uncompressed and compressed sound alike: MACE, IMA4,
//! QDesign, GSM and PCM tracks give every sample a 1-byte `stsz` size, which
//! is not the size of anything that can be decoded. FFmpeg's MOV demuxer
//! then builds its index per chunk instead, and so does this demuxer:
//! - frames of `samplesPerPacket` samples and `bytesPerFrame` bytes (sound
//!   description version 1, or the version 2 constants), with the fixed
//!   values of MACE 3:1 (6 samples, 2 bytes per channel), MACE 6:1 (6, 1),
//!   IMA4 (64, 34 per channel), GSM (160, 33) and QCELP (160, 35);
//! - a packet is one frame for frames of 160 samples or more, else as many
//!   whole frames as fit in 1024 samples, or for PCM up to 1024 samples of
//!   the sample size (bits per sample times channels, else the `stsz` size);
//! - packets follow each other in their chunk; dts count samples from 0.
//!
//! FFmpeg's limits hold too: a chunk whose sample count is not a multiple
//! of the frame (but in the last `stsc` entry) or more packets than its
//! count leave the index there, and so does a frame size of 0 or past 1 GiB.

use crate::sample_table::SampleEntry;
use crate::track::{SoundV2, Track};

/// `samples_per_frame`, `bytes_per_frame` and `sample_size` of FFmpeg's
/// `MOVStreamContext` for a sound track.
#[derive(Debug, PartialEq, Eq)]
struct Framing {
    samples_per_frame: u32,
    bytes_per_frame: u32,
    sample_size: u32,
}

/// `av_get_bits_per_sample` of the codec FFmpeg maps a sound format to,
/// after `mov_parse_stsd_audio`'s bit-depth adjustments; 0 for compressed
/// codecs and those of fewer than 8 bits.
fn pcm_bits(format: &[u8; 4], bits: u16, v2: Option<&SoundV2>) -> u32 {
    match format {
        // `raw ` and `NONE` are unsigned 8-bit, big-endian 16-bit when the
        // entry says 16; format 0 is `raw ` at 8 bits and `twos` at 16.
        b"raw " | b"NONE" => match bits {
            16 => 16,
            _ => 8,
        },
        b"\0\0\0\0" => match bits {
            8 | 16 => u32::from(bits),
            _ => 0,
        },
        b"twos" | b"sowt" => match bits {
            8 => 8,
            24 => 24,
            32 => 32,
            _ => 16,
        },
        b"in24" | b"42ni" => 24,
        b"in32" | b"23ni" | b"fl32" => 32,
        b"fl64" => 64,
        b"alaw" | b"ulaw" => 8,
        b"lpcm" => v2.map_or(0, |v2| v2.const_bits_per_channel),
        _ => 0,
    }
}

fn framing(t: &Track) -> Framing {
    let stsz = t.sample_table.stsz_default_size.unwrap_or(0);
    let Some(d) = t.sample_descriptions.first() else {
        return Framing { samples_per_frame: 0, bytes_per_frame: 0, sample_size: stsz };
    };
    let channels = u32::from(d.channels);
    let (mut spf, mut bpf) = match (&d.sound_v1, &d.sound_v2) {
        (Some(v1), _) => (v1.samples_per_packet, v1.bytes_per_frame),
        (None, Some(v2)) => (v2.const_lpcm_frames_per_audio_packet, v2.const_bytes_per_audio_packet),
        (None, None) => (0, 0),
    };
    match &d.format {
        b"MAC3" => (spf, bpf) = (6, 2 * channels),
        b"MAC6" => (spf, bpf) = (6, channels),
        b"ima4" => (spf, bpf) = (64, 34 * channels),
        b"agsm" => (spf, bpf) = (160, 33),
        b"Qclp" | b"Qclq" | b"sqcp" => {
            spf = 160;
            if bpf == 0 {
                bpf = 35;
            }
        }
        _ => {}
    }
    let bits = pcm_bits(&d.format, d.bits_per_sample, d.sound_v2.as_ref());
    let sample_size = if bits >= 8 { (bits / 8).saturating_mul(channels) } else { stsz };
    Framing { samples_per_frame: spf, bytes_per_frame: bpf, sample_size }
}

/// The packets FFmpeg reads from `t`, if FFmpeg groups its samples by chunk
/// (an audio track whose `stts` is one entry of duration 1); at most
/// `max_entries` (none when there would be more).
pub(crate) fn grouped_samples(t: &Track, max_entries: u64) -> Option<Vec<SampleEntry>> {
    let table = &t.sample_table;
    if !t.is_audio() || table.stts.len() != 1 || table.stts[0].sample_duration != 1 {
        return None;
    }
    let chunk_count = table.chunk_offsets.len() as u32;
    if chunk_count == 0 || table.stsc.is_empty() {
        return Some(Vec::new());
    }
    let Framing { samples_per_frame: spf, bytes_per_frame: bpf, sample_size } = framing(t);

    // The packet count, in FFmpeg's unsigned arithmetic.
    let mut total: u32 = 0;
    for (i, e) in table.stsc.iter().enumerate() {
        let chunk_samples = e.samples_per_chunk;
        if i != table.stsc.len() - 1 && spf != 0 && chunk_samples % spf != 0 {
            return Some(Vec::new()); // "error unaligned chunk"
        }
        let count = if spf >= 160 {
            chunk_samples / spf
        } else if spf > 1 {
            let samples = (1024 / spf) * spf;
            chunk_samples.wrapping_add(samples - 1) / samples
        } else {
            chunk_samples.wrapping_add(1023) / 1024
        };
        let chunks = match table.stsc.get(i + 1) {
            Some(next) => next.first_chunk.wrapping_sub(e.first_chunk),
            None => chunk_count.wrapping_sub(e.first_chunk.wrapping_sub(1)),
        };
        total = total.wrapping_add(chunks.wrapping_mul(count));
    }
    if u64::from(total) >= u64::from(u32::MAX) / 24 || u64::from(total) > max_entries {
        return Some(Vec::new());
    }

    let mut out: Vec<SampleEntry> = Vec::with_capacity(total as usize);
    let mut stsc_index = 0usize;
    let mut dts: u64 = 0;
    'chunks: for i in 0..chunk_count {
        let mut offset = table.chunk_offsets[i as usize];
        if table.stsc.get(stsc_index + 1).is_some_and(|next| i + 1 == next.first_chunk) {
            stsc_index += 1;
        }
        let entry = table.stsc[stsc_index];
        let mut chunk_samples = entry.samples_per_chunk;
        while chunk_samples > 0 {
            if spf > 1 && bpf == 0 {
                break 'chunks;
            }
            let (samples, size) = if spf >= 160 {
                (spf, bpf)
            } else if spf > 1 {
                let samples = ((1024 / spf) * spf).min(chunk_samples);
                (samples, (samples / spf).wrapping_mul(bpf))
            } else {
                let samples = chunk_samples.min(1024);
                (samples, samples.wrapping_mul(sample_size))
            };
            if out.len() >= total as usize || size > 0x3FFF_FFFF {
                break 'chunks; // "wrong chunk count", "Sample size too large"
            }
            out.push(SampleEntry {
                index: out.len() as u32,
                offset,
                size,
                dts,
                duration: samples,
                sample_description_id: entry.sample_description_id,
                keyframe: true,
                composition_offset: 0,
            });
            offset = offset.saturating_add(u64::from(size));
            dts += u64::from(samples);
            chunk_samples = chunk_samples.wrapping_sub(samples);
        }
    }
    // A packet lasts until the next one starts, the last until the end of
    // the media (FFmpeg's mov_read_packet, edit lists ignored).
    let mut until = t.mdhd.duration;
    for e in out.iter_mut().rev() {
        e.duration = until.checked_sub(e.dts).map_or(0, |d| u32::try_from(d).unwrap_or(u32::MAX));
        until = e.dts;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_bits_follow_ffmpegs_codec_mapping() {
        assert_eq!(pcm_bits(b"twos", 16, None), 16);
        assert_eq!(pcm_bits(b"twos", 8, None), 8);
        assert_eq!(pcm_bits(b"sowt", 24, None), 24);
        assert_eq!(pcm_bits(b"raw ", 8, None), 8);
        assert_eq!(pcm_bits(b"raw ", 16, None), 16);
        assert_eq!(pcm_bits(b"\0\0\0\0", 16, None), 16);
        assert_eq!(pcm_bits(b"\0\0\0\0", 4, None), 0);
        assert_eq!(pcm_bits(b"alaw", 16, None), 8);
        assert_eq!(pcm_bits(b"ima4", 16, None), 0);
        assert_eq!(pcm_bits(b"MAC3", 8, None), 0);
    }
}
