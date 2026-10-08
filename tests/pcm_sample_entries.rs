//! Linear PCM and G.711 sound entries get the codec FFmpeg 2da55bf's MOV
//! demuxer gives them (mov.c: the `ff_codec_movaudio_tags` format, the
//! version 2 `lpcm` flags, the bit depth, a little-endian `enda`), with no
//! registry claim on the format, so the PCM and G.711 decoders play them.
#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{NullCodecResolver, ReadSeek};

/// A one-sample sound movie whose `stsd` holds `entry` (a whole entry).
fn movie(entry: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &[b"qt  ".as_slice(), &0u32.to_be_bytes(), b"qt  "].concat());
    let offset = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &[0; 64]);
    let mut stsd = [0u32.to_be_bytes(), 1u32.to_be_bytes()].concat();
    stsd.extend_from_slice(entry);
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &stsd);
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 1));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(64, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(offset));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(44_100, 1));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 0, 0));
    push_atom(&mut trak, *b"mdia", &mdia);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 1));
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

/// A sound sample entry of `version`: the 20 bytes every version has,
/// then `tail` (the version's fields and extension atoms).
fn entry(format: &[u8; 4], version: u16, bits: u16, tail: &[u8]) -> Vec<u8> {
    let mut e = ((16 + 20 + tail.len()) as u32).to_be_bytes().to_vec();
    e.extend_from_slice(format);
    e.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    let mut body = [0u8; 20];
    body[0..2].copy_from_slice(&version.to_be_bytes());
    body[8..10].copy_from_slice(&2u16.to_be_bytes());
    body[10..12].copy_from_slice(&bits.to_be_bytes());
    body[16..20].copy_from_slice(&(44_100u32 << 16).to_be_bytes());
    e.extend_from_slice(&body);
    e.extend_from_slice(tail);
    e
}

/// A version 1 entry whose `wave` atom carries `enda` = `little`.
fn v1_with_enda(format: &[u8; 4], bits: u16, little: u16) -> Vec<u8> {
    let mut wave = Vec::new();
    push_atom(&mut wave, *b"frma", format);
    push_atom(&mut wave, *b"enda", &little.to_be_bytes());
    push_atom(&mut wave, [0; 4], &[]);
    let mut tail = [1u32, 4, 8, 4].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
    push_atom(&mut tail, *b"wave", &wave);
    entry(format, 1, bits, &tail)
}

/// A version 2 `lpcm` entry of `bits` with the format-specific `flags`.
fn v2_lpcm(bits: u32, flags: u32) -> Vec<u8> {
    let mut tail = 72u32.to_be_bytes().to_vec();
    tail.extend_from_slice(&44_100f64.to_bits().to_be_bytes());
    for v in [2u32, 0x7F00_0000, bits, flags, bits / 8 * 2, 1] {
        tail.extend_from_slice(&v.to_be_bytes());
    }
    entry(b"lpcm", 2, 16, &tail)
}

fn codec(entry: Vec<u8>) -> String {
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(movie(&entry)));
    let d = oxideav_mov::demuxer::open(input, &NullCodecResolver).expect("open");
    d.streams()[0].params.codec_id.as_str().to_string()
}

#[test]
fn version_0_formats_by_their_bit_depth() {
    for (format, bits, want) in [
        (b"twos", 16, "pcm_s16be"),
        (b"twos", 8, "pcm_s8"),
        (b"twos", 24, "pcm_s24be"),
        (b"twos", 32, "pcm_s32be"),
        (b"sowt", 16, "pcm_s16le"),
        (b"sowt", 8, "pcm_s8"),
        (b"sowt", 24, "pcm_s24le"),
        (b"sowt", 32, "pcm_s32le"),
        (b"raw ", 8, "pcm_u8"),
        (b"raw ", 16, "pcm_s16be"),
        (b"NONE", 8, "pcm_u8"),
        (b"in24", 24, "pcm_s24be"),
        (b"42ni", 24, "pcm_s24le"),
        (b"in32", 32, "pcm_s32be"),
        (b"23ni", 32, "pcm_s32le"),
        (b"fl32", 32, "pcm_f32be"),
        (b"fl64", 64, "pcm_f64be"),
        (&[0; 4], 8, "pcm_u8"),
        (&[0; 4], 16, "pcm_s16be"),
        (b"alaw", 16, "pcm_alaw"),
        (b"ulaw", 16, "pcm_mulaw"),
    ] {
        assert_eq!(codec(entry(format, 0, bits, &[])), want, "{} at {bits} bits", String::from_utf8_lossy(format));
    }
}

#[test]
fn enda_makes_them_little_endian() {
    for (format, bits, little, want) in [
        (b"in24", 24, 1, "pcm_s24le"),
        (b"in32", 32, 1, "pcm_s32le"),
        (b"fl32", 32, 1, "pcm_f32le"),
        (b"fl64", 64, 1, "pcm_f64le"),
        (b"twos", 16, 1, "pcm_s16le"),
        (b"in24", 24, 0, "pcm_s24be"),
    ] {
        assert_eq!(codec(v1_with_enda(format, bits, little)), want, "{} enda {little}", String::from_utf8_lossy(format));
    }
}

/// CoreAudio flags: 1 float, 2 big-endian, 4 signed integer.
#[test]
fn version_2_lpcm_by_its_flags() {
    for (bits, flags, want) in [
        (32, 1 | 2, "pcm_f32be"),
        (64, 1, "pcm_f64le"),
        (24, 4, "pcm_s24le"),
        (16, 4 | 2, "pcm_s16be"),
        (32, 4, "pcm_s32le"),
        (64, 4 | 2, "pcm_s64be"),
        (8, 0, "pcm_u8"),
        (16, 0, "pcm_u16le"),
    ] {
        assert_eq!(codec(v2_lpcm(bits, flags)), want, "{bits} bits, flags {flags}");
    }
}

#[test]
fn other_formats_still_go_to_the_registry() {
    assert_eq!(codec(entry(b"ima4", 0, 16, &[])), "unknown");
}
