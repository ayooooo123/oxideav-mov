//! Opened through the registry, a movie's edit lists read as FFmpeg 2da55bf's
//! mov demuxer reads the lists it applies as one shift (leading empty edits,
//! then one media edit at rate 1): timestamps move by the empty edits minus
//! the edit's `media_time`, and audio packets carry what FFmpeg trims from
//! the decoded sound as `PacketMetadata::audio_trim` — the priming before
//! `media_time`, and the end past the edit.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{AudioTrim, CodecId, CodecResolver, CodecTag, Demuxer, Error, ProbeContext, ReadSeek};

/// Resolves the two sound formats these movies use.
struct Tags;

impl CodecResolver for Tags {
    fn resolve_tag(&self, ctx: &ProbeContext) -> Option<CodecId> {
        let CodecTag::Fourcc(fourcc) = ctx.tag else { return None };
        match fourcc {
            b"MP4A" => Some(CodecId::new("aac")),
            b"TWOS" => Some(CodecId::new("pcm_s16be")),
            b"AGSM" => Some(CodecId::new("gsm")),
            _ => None,
        }
    }
}

fn table(entries: &[[u32; 2]]) -> Vec<u8> {
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        p.extend_from_slice(&e[0].to_be_bytes());
        p.extend_from_slice(&e[1].to_be_bytes());
    }
    p
}

/// An `edts/elst` of `(duration in the movie timescale, media_time)` edits.
fn edts(edits: &[(u32, i32)]) -> Vec<u8> {
    let mut elst = vec![0u8; 4];
    elst.extend_from_slice(&(edits.len() as u32).to_be_bytes());
    for &(duration, media_time) in edits {
        elst.extend_from_slice(&duration.to_be_bytes());
        elst.extend_from_slice(&media_time.to_be_bytes());
        elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    }
    let mut out = Vec::new();
    push_atom(&mut out, *b"elst", &elst);
    out
}

/// A one-track sound movie (movie timescale 48000) whose samples sit in one
/// chunk at the start of `mdat`.
fn movie(stsd: &[u8], stts: &[[u32; 2]], stsz: &[u8], per_chunk: u32, mdhd: (u32, u32), edits: &[(u32, i32)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &[b"qt  ".as_slice(), &0u32.to_be_bytes(), b"qt  "].concat());
    let offset = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &vec![0u8; 16384]);
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", stsd);
    push_atom(&mut stbl, *b"stts", &table(stts));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(per_chunk));
    push_atom(&mut stbl, *b"stsz", stsz);
    push_atom(&mut stbl, *b"stco", &build_stco_single(offset));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(mdhd.0, mdhd.1));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 0, 0));
    push_atom(&mut trak, *b"edts", &edts(edits));
    push_atom(&mut trak, *b"mdia", &mdia);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(48000, 0));
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

/// AAC as FFmpeg's muxer writes it: 1024-sample packets, the last 768 long,
/// one edit skipping the 1024 priming samples and ending 768 samples before
/// the media does.
fn aac_movie() -> Vec<u8> {
    let mut stsz = vec![0u8; 8];
    stsz.extend_from_slice(&4u32.to_be_bytes());
    for _ in 0..4 {
        stsz.extend_from_slice(&16u32.to_be_bytes());
    }
    let stsd = build_stsd_audio(b"mp4a", 2, 16, 48000, &[]);
    movie(&stsd, &[[3, 1024], [1, 768]], &stsz, 4, (48000, 3840), &[(2304, 1024)])
}

/// `(pts, trim)` of every packet, read as the player reads them.
fn packets(mut d: Box<dyn Demuxer>) -> Vec<(Option<i64>, Option<AudioTrim>)> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push((p.pts, d.packet_metadata().audio_trim)),
            Err(Error::Eof) => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

fn trim(skip: u32, discard: u32) -> Option<AudioTrim> {
    Some(AudioTrim { skip_samples: skip, discard_padding: discard, sample_rate: 48000 })
}

#[test]
fn aac_priming_and_end_are_trimmed_and_timestamps_shift() {
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(aac_movie()));
    let d = oxideav_mov::demuxer::open(input, &Tags).expect("open");
    // The last packet presents 256 of its 768 samples and decodes as a
    // whole 1024-sample AAC frame: 768 come off.
    assert_eq!(packets(d), [(Some(-1024), trim(1024, 0)), (Some(0), None), (Some(1024), None), (Some(2048), trim(0, 768))]);
}

#[test]
fn a_seek_to_the_start_skips_the_priming_again() {
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(aac_movie()));
    let mut d = oxideav_mov::demuxer::open(input, &Tags).expect("open");
    while d.next_packet().is_ok() {}
    assert_eq!(d.seek_to(0, -1024).expect("seek"), -1024);
    d.next_packet().expect("the first packet");
    assert_eq!(d.packet_metadata().audio_trim, trim(1024, 0));
}

#[test]
fn a_chunk_grouped_track_loses_the_packets_past_its_edit() {
    // 2560 16-bit stereo samples in one chunk: packets of 1024, 1024, 512.
    // The edit ends at 1500: FFmpeg reads no packet after the one that
    // reaches it, and that one, cut to the stream's end, discards nothing.
    let stsd = build_stsd_audio(b"twos", 2, 16, 48000, &[]);
    let stsz = build_stsz_constant(1, 2560);
    let file = movie(&stsd, &[[2560, 1]], &stsz, 2560, (48000, 2560), &[(1500, 0)]);
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = oxideav_mov::demuxer::open(input, &Tags).expect("open");
    assert_eq!(packets(d), [(Some(0), None), (Some(1024), None), (Some(2048), trim(0, 512))]);
}

#[test]
fn a_gsm_frame_past_the_edit_end_is_discarded() {
    // 800 GSM samples in one chunk: one 33-byte packet per 160-sample frame.
    // The edit ends at 789: the last frame presents 149 samples, and
    // demux.c counts it as GSM's 160 (`codecpar->frame_size`), so 11 come
    // off, as FFmpeg trims fate-suite gsm/sample-gsm-8000.mov.
    let stsd = build_stsd_audio(b"agsm", 1, 16, 48000, &[]);
    let stsz = build_stsz_constant(1, 800);
    let file = movie(&stsd, &[[800, 1]], &stsz, 800, (48000, 800), &[(789, 0)]);
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let d = oxideav_mov::demuxer::open(input, &Tags).expect("open");
    assert_eq!(
        packets(d),
        [(Some(0), None), (Some(160), None), (Some(320), None), (Some(480), None), (Some(640), trim(0, 11))]
    );
}
