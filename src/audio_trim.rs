// SPDX-License-Identifier: LGPL-2.1-or-later
// Port of FFmpeg 2da55bf libavformat/mov.c (mov_fix_index's shift, priming
// and end for one media edit; mov_read_packet's last-packet duration and
// discard window; mov_get_skip_samples) and libavformat/demux.c (the discard
// window of read_frame_internal).
// Copyright (c) 2001 Fabrice Bellard, 2009 Baptiste Coudurier (mov.c);
// 2000-2002 Fabrice Bellard (demux.c)
//
// This file is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the Free
// Software Foundation; either version 2.1, or (at your option) any later version.
// It is distributed WITHOUT ANY WARRANTY; without even the implied warranty
// of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See LICENSE-LGPL.

//! FFmpeg's reading of an edit list it applies as one shift: leading empty
//! edits, then one media edit at rate 1 (what phones, QuickTime and FFmpeg's
//! muxer write for a start delay, B-frame reordering or AAC priming).
//! Other lists keep the media timeline.
//!
//! - Every timestamp of the track moves by the empty edits' duration minus
//!   the media edit's `media_time`.
//! - Audio: the samples before `media_time` are priming; the track's first
//!   packet skips them, whole packets and the part of the one that
//!   straddles the edit start (not for Vorbis). FFmpeg reads no packet
//!   after the first one that reaches the edit's end; here they stay, and
//!   discard all they decode to. The last packet FFmpeg reads discards
//!   what lies past the stream's duration (the least of `mdhd`, the `stts`
//!   total and the edit list's), counted as at least one codec frame long
//!   for codecs with fixed frames. In a track read per chunk
//!   (`sound_chunks`) that packet lasts until the stream's end, so it
//!   discards nothing.
//! - After a seek, an audio track's next packet skips the priming still
//!   ahead of it.
//!
//! Counts are in the media timescale, which the trim declares as its rate.
//! Tracks with movie fragments get no trims.

use std::collections::{BTreeMap, HashMap};

use crate::sample_table::SampleEntry;
use crate::track::Track;

/// A packet's trims, in its track's media timescale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trim {
    pub(crate) skip: u32,
    pub(crate) discard: u32,
}

/// What an audio track's first packet after a seek skips.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SeekTrim {
    /// The track's priming (FFmpeg's `initial_padding`).
    initial_padding: i64,
    /// The media dts of the track's first packet.
    first_dts: i64,
}

impl SeekTrim {
    /// The skip of a packet at media `dts` (`mov_get_skip_samples`).
    pub(crate) fn skip_at(&self, dts: i64) -> u32 {
        clamp_u32(self.initial_padding.saturating_sub(dts.saturating_sub(self.first_dts)))
    }
}

fn clamp_u32(v: i64) -> u32 {
    v.clamp(0, i64::from(u32::MAX)) as u32
}

/// `a * b / c` rounded to nearest, halves away from zero (`av_rescale`),
/// saturating; `c` must be positive.
fn rescale(a: i64, b: i64, c: i64) -> i64 {
    let n = i128::from(a) * i128::from(b);
    let c = i128::from(c);
    let r = if n >= 0 { (n + c / 2) / c } else { (n - c / 2) / c };
    r.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// The media edit of a list FFmpeg applies as one shift, in the media
/// timescale.
struct MediaEdit {
    /// The leading empty edits' duration.
    empty: i64,
    media_time: i64,
    /// 0: open-ended.
    duration: i64,
}

fn media_edit(t: &Track, movie_timescale: u32) -> Option<MediaEdit> {
    let media_timescale = i64::from(t.mdhd.time_scale);
    if movie_timescale == 0 || media_timescale == 0 {
        return None;
    }
    let mut empty = 0i64;
    let mut edit = None;
    for e in &t.edits {
        if edit.is_some() {
            return None;
        }
        let raw = i64::try_from(e.track_duration).unwrap_or(i64::MAX);
        let mut duration = rescale(raw, media_timescale, i64::from(movie_timescale));
        if duration.checked_add(e.media_time.max(0)).is_none() {
            duration = 0;
        }
        if e.is_empty() {
            empty = empty.saturating_add(duration);
        } else if e.media_rate == 0x0001_0000 {
            edit = Some(MediaEdit { empty, media_time: e.media_time, duration });
        } else {
            return None;
        }
    }
    edit
}

/// How far FFmpeg moves `t`'s timestamps, in its media timescale: 0 unless
/// its edit list is one FFmpeg applies as one shift.
pub(crate) fn shift(t: &Track, movie_timescale: u32) -> i64 {
    media_edit(t, movie_timescale).map_or(0, |e| e.empty.saturating_sub(e.media_time))
}

/// The codec frame length FFmpeg's decoders report for codecs with fixed
/// frames (the track's most common sample duration), 0 for the others.
fn frame_size(t: &Track, codec: &str) -> i64 {
    if !matches!(codec, "aac" | "mp1" | "mp2" | "mp3" | "ac3" | "eac3") {
        return 0;
    }
    let mut counts: BTreeMap<u32, u64> = BTreeMap::new();
    for e in &t.sample_table.stts {
        *counts.entry(e.sample_duration).or_default() += u64::from(e.sample_count);
    }
    counts.into_iter().max_by_key(|&(delta, count)| (count, delta)).map_or(0, |(delta, _)| i64::from(delta))
}

/// Stores the trims of audio track `track`, whose `packets` are in decode
/// order on the media timeline, in `trims` (keyed by track and sample
/// index), and returns what a seek needs. `grouped`: the track is read per
/// chunk.
pub(crate) fn audio_trims(
    t: &Track,
    track: u32,
    codec: &str,
    movie_timescale: u32,
    grouped: bool,
    packets: &[(u32, SampleEntry)],
    trims: &mut HashMap<(u32, u32), Trim>,
) -> Option<SeekTrim> {
    if !t.is_audio() || packets.is_empty() || !t.fragment_samples.is_empty() {
        return None;
    }
    let edit = media_edit(t, movie_timescale)?;
    let shift = edit.empty.saturating_sub(edit.media_time);
    let cts = |k: usize| packets[k].1.pts();
    let pts = |k: usize| cts(k).saturating_add(shift);
    // FFmpeg's frame duration: the dts step to the next packet, and the
    // edit's duration for the last one.
    let step = |k: usize| match packets.get(k + 1) {
        Some((_, next)) => (next.dts as i64).saturating_sub(packets[k].1.dts as i64),
        None => edit.duration,
    };

    let mut skip = 0i64;
    if codec != "vorbis" {
        for k in 0..packets.len() {
            let c = cts(k);
            if c >= edit.media_time {
                break;
            }
            let frame = step(k);
            if c.saturating_add(frame) > edit.media_time {
                skip = skip.saturating_add(edit.media_time - c);
                break;
            }
            skip = skip.saturating_add(frame);
        }
    }
    let mut keep = packets.len();
    if edit.duration > 0 {
        let end = edit.media_time.saturating_add(edit.duration);
        if let Some(k) = (0..packets.len()).find(|&k| cts(k).saturating_add(step(k)) >= end) {
            keep = k + 1;
        }
    }

    // st->duration: mdhd, then the stts total, then the edit list's length.
    let mdhd = t.mdhd.duration;
    let mut total = if mdhd == u64::from(u32::MAX) || mdhd == u64::MAX { 0 } else { i64::try_from(mdhd).unwrap_or(i64::MAX) };
    let stts_total = t
        .sample_table
        .stts
        .iter()
        .fold(0i64, |sum, e| sum.saturating_add(i64::from(e.sample_count).saturating_mul(i64::from(e.sample_duration))));
    if stts_total > 0 {
        total = total.min(stts_total);
    }
    total = total.min(edit.empty.saturating_add(edit.duration).max(0));

    let frame = frame_size(t, codec);
    let key = |k: usize| (track, packets[k].1.index);

    // The last packet FFmpeg reads: its duration is the stts one, or in a
    // track read per chunk the time to the stream's end.
    let last = keep - 1;
    let mut duration = i64::from(packets[last].1.duration);
    if grouped {
        duration = duration.min(total.saturating_sub(pts(last)).max(0));
    }
    let presented = if total < pts(last) { 0 } else { duration.min(total - pts(last)) };
    let first_discard = pts(last).saturating_add(presented);
    if first_discard != 0 {
        let length = frame.max(duration);
        let end = pts(last).saturating_add(length);
        if length > 0 && end > first_discard && pts(last) < total {
            trims.entry(key(last)).or_default().discard = clamp_u32((end - first_discard).min(length));
        }
    }
    for (_, s) in &packets[keep..] {
        let length = frame.max(i64::from(s.duration));
        if length > 0 {
            trims.entry((track, s.index)).or_default().discard = clamp_u32(length);
        }
    }
    if skip > 0 {
        trims.entry(key(0)).or_default().skip = clamp_u32(skip);
    }
    Some(SeekTrim { initial_padding: skip, first_dts: packets[0].1.dts as i64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescale_rounds_halves_away_from_zero() {
        assert_eq!(rescale(6000, 48000, 1000), 288000);
        assert_eq!(rescale(1, 3, 2), 2);
        assert_eq!(rescale(-1, 3, 2), -2);
        assert_eq!(rescale(i64::MAX, 4, 1), i64::MAX);
    }
}
