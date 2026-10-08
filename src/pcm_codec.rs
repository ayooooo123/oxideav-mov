// SPDX-License-Identifier: LGPL-2.1-or-later
// Port of FFmpeg 2da55bf libavformat/mov.c: the linear PCM codec of a sound
// sample entry as mov_codec_id (with the PCM entries of isom_tags.c
// ff_codec_movaudio_tags), mov_parse_stsd_audio (version 2 `lpcm` through
// isom.h ff_mov_get_lpcm_codec_id and utils.c ff_get_pcm_codec_id; the
// bit-depth switch) and mov_read_enda / set_last_stream_little_endian choose
// it.
// Copyright (c) 2001 Fabrice Bellard; Copyright (c) 2009 Baptiste Coudurier
// (mov.c).
//
// This file is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the Free
// Software Foundation; either version 2.1, or (at your option) any later version.
// It is distributed WITHOUT ANY WARRANTY; without even the implied warranty
// of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See LICENSE-LGPL.

//! FFmpeg's MOV demuxer, not a decoder's tag claim, decides which PCM a
//! QuickTime sound entry holds, and so does this demuxer:
//! - the format's codec: `twos` signed 16-bit big-endian, `sowt` little-
//!   endian, `in24`/`in32` signed 24/32-bit big-endian (`42ni`/`23ni`
//!   little-endian), `fl32`/`fl64` big-endian float, `raw `/`NONE` unsigned
//!   8-bit, format 0 `raw ` at 8 bits and `twos` at 16, `lpcm` big-endian
//!   16-bit, `alaw`/`ulaw` G.711;
//! - a version 2 `lpcm` entry by its format-specific flags (1 float, 2
//!   big-endian, 4 signed integer) and bits per channel;
//! - the entry's bit depth: 8-bit `twos`/`sowt` are signed 8-bit, 24 and
//!   32-bit ones signed 24/32-bit; 16-bit `raw `/`NONE` signed 16-bit
//!   big-endian;
//! - an `enda` atom of 1 (in `wave`, or among the entry's extension atoms)
//!   turns the big-endian codecs little-endian.

use crate::track::SampleDescription;

/// The PCM codec id FFmpeg's MOV demuxer gives the sound entry `desc`;
/// None for an entry of another codec.
pub(crate) fn pcm_codec_id(desc: &SampleDescription) -> Option<&'static str> {
    let bits = desc.bits_per_sample;
    let mut id = match &desc.format {
        b"twos" | b"lpcm" => "pcm_s16be",
        b"sowt" => "pcm_s16le",
        b"in24" => "pcm_s24be",
        b"42ni" => "pcm_s24le",
        b"in32" => "pcm_s32be",
        b"23ni" => "pcm_s32le",
        b"fl32" => "pcm_f32be",
        b"fl64" => "pcm_f64be",
        b"raw " | b"NONE" => "pcm_u8",
        b"alaw" => "pcm_alaw",
        b"ulaw" => "pcm_mulaw",
        b"\0\0\0\0" => match bits {
            8 => "pcm_u8",
            16 => "pcm_s16be",
            _ => return None,
        },
        _ => return None,
    };
    if &desc.format == b"lpcm" {
        if let Some(v2) = &desc.sound_v2 {
            id = lpcm_codec_id(v2.const_bits_per_channel, v2.format_specific_flags.0)?;
        }
    }
    id = match (id, bits) {
        ("pcm_s8" | "pcm_u8", 16) => "pcm_s16be",
        ("pcm_s16le" | "pcm_s16be", 8) => "pcm_s8",
        ("pcm_s16be", 24) => "pcm_s24be",
        ("pcm_s16le", 24) => "pcm_s24le",
        ("pcm_s16be", 32) => "pcm_s32be",
        ("pcm_s16le", 32) => "pcm_s32le",
        (id, _) => id,
    };
    if little_endian(desc) {
        id = match id {
            "pcm_s16be" => "pcm_s16le",
            "pcm_s24be" => "pcm_s24le",
            "pcm_s32be" => "pcm_s32le",
            "pcm_f32be" => "pcm_f32le",
            "pcm_f64be" => "pcm_f64le",
            id => id,
        };
    }
    Some(id)
}

/// ff_mov_get_lpcm_codec_id: `bits` per channel with CoreAudio's flags.
fn lpcm_codec_id(bits: u32, flags: u32) -> Option<&'static str> {
    if bits == 0 || bits > 64 {
        return None;
    }
    let (float, big, signed) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
    let pick = |be: &'static str, le: &'static str| Some(if big { be } else { le });
    if float {
        return match bits {
            32 => pick("pcm_f32be", "pcm_f32le"),
            64 => pick("pcm_f64be", "pcm_f64le"),
            _ => None,
        };
    }
    match ((bits + 7) / 8, signed) {
        (1, true) => Some("pcm_s8"),
        (2, true) => pick("pcm_s16be", "pcm_s16le"),
        (3, true) => pick("pcm_s24be", "pcm_s24le"),
        (4, true) => pick("pcm_s32be", "pcm_s32le"),
        (8, true) => pick("pcm_s64be", "pcm_s64le"),
        (1, false) => Some("pcm_u8"),
        (2, false) => pick("pcm_u16be", "pcm_u16le"),
        (3, false) => pick("pcm_u24be", "pcm_u24le"),
        (4, false) => pick("pcm_u32be", "pcm_u32le"),
        _ => None,
    }
}

/// mov_read_enda: an `enda` atom whose low byte is 1, in the entry's `wave`
/// atom or among its extension atoms.
fn little_endian(desc: &SampleDescription) -> bool {
    let is_little = |payload: &[u8]| payload.get(1) == Some(&1);
    if let Some(wave) = &desc.si_decompression_param {
        if wave.child(b"enda").is_some_and(is_little) {
            return true;
        }
    }
    let mut extra = desc.extra.as_slice();
    while extra.len() >= 8 {
        let size = u32::from_be_bytes([extra[0], extra[1], extra[2], extra[3]]) as usize;
        if size < 8 || size > extra.len() {
            break;
        }
        if &extra[4..8] == b"enda" && is_little(&extra[8..size]) {
            return true;
        }
        extra = &extra[size..];
    }
    false
}
