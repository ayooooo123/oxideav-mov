//! AMR is mono at the AMR rate. 3GPP fixes an AMR sample entry's channel
//! count at 2 (TS 26.244 §6.5); FFmpeg 2da55bf's mov demuxer forces mono and
//! 8000 Hz (AMR-NB) or 16000 Hz (AMR-WB) (`mov_finalize_stsd_codec`), and
//! so does this one.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{CodecId, CodecResolver, CodecTag, ProbeContext, ReadSeek};

/// Resolves the two AMR entries as the AMR decoders claim them.
struct Amr;

impl CodecResolver for Amr {
    fn resolve_tag(&self, ctx: &ProbeContext) -> Option<CodecId> {
        let CodecTag::Fourcc(fourcc) = ctx.tag else { return None };
        match fourcc {
            b"SAMR" => Some(CodecId::new("amr_nb")),
            b"SAWB" => Some(CodecId::new("amr_wb")),
            _ => None,
        }
    }
}

/// A one-frame AMR movie: a `format` entry saying 2 channels and `rate`.
fn movie(format: &[u8; 4], rate: u32) -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &[b"qt  ".as_slice(), &0u32.to_be_bytes(), b"qt  "].concat());
    let offset = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &[0x3C; 13]);
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd_audio(format, 2, 16, rate, &[]));
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 160));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(13, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(offset));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(8000, 160));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 0, 0, 0));
    push_atom(&mut trak, *b"mdia", &mdia);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 12));
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn amr_entries_are_mono_at_the_amr_rate() {
    for (format, entry_rate, codec, rate) in [(b"samr", 8000, "amr_nb", 8000), (b"sawb", 16000, "amr_wb", 16000), (b"samr", 0, "amr_nb", 8000)] {
        let input: Box<dyn ReadSeek> = Box::new(Cursor::new(movie(format, entry_rate)));
        let d = oxideav_mov::demuxer::open(input, &Amr).expect("open");
        let params = &d.streams()[0].params;
        assert_eq!(params.codec_id.as_str(), codec);
        assert_eq!((params.channels, params.sample_rate), (Some(1), Some(rate)), "{codec} entry at {entry_rate} Hz");
    }
}
