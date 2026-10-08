//! A video track's decoder configuration is the payload of its sample
//! entry's configuration atom, as FFmpeg 2da55bf's mov demuxer reads it into
//! extradata (`mov_read_glbl`, `mov_read_esds`): `avcC` for H.264, `hvcC`
//! for HEVC, the DecoderSpecificInfo of an `esds` for MPEG-4 video. The
//! other atoms of the extension area (`pasp`, `colr`, ...) are not part of
//! it, and neither are the atom headers.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, ReadSeek};
use oxideav_mov::MovDemuxer;

/// AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1): one SPS, one
/// PPS.
const AVCC: [u8; 19] =
    [1, 0x64, 0x00, 0x1F, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x1F, 0x01, 0x00, 0x04, 0x68, 0xEE, 0x3C, 0x80];

/// HEVCDecoderConfigurationRecord (§8.3.3.1) with no parameter-set arrays.
const HVCC: [u8; 23] =
    [1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5D, 0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0x00, 0x00, 0x0F, 0x00];

const PASP: [u8; 8] = [0, 0, 0, 1, 0, 0, 0, 1];
const COLR: [u8; 10] = *b"nclc\0\x01\0\x01\0\x01";

/// A one-frame 320x240 movie whose video sample entry is `format` with the
/// extension-area `atoms`.
fn movie(format: &[u8; 4], atoms: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut extension = Vec::new();
    for (fourcc, body) in atoms {
        push_atom(&mut extension, *fourcc, body);
    }
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &[b"qt  ".as_slice(), &0u32.to_be_bytes(), b"qt  "].concat());
    let offset = (out.len() + 8) as u32;
    push_atom(&mut out, *b"mdat", &[0u8; 4]);
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd_video(format, 320, 240, &extension));
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 512));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(4, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(offset));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"vmhd", &build_vmhd());
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(15360, 512));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"vide"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(1, 20, 320, 240));
    push_atom(&mut trak, *b"mdia", &mdia);
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 20));
    push_atom(&mut moov, *b"trak", &trak);
    push_atom(&mut out, *b"moov", &moov);
    out
}

fn extradata(format: &[u8; 4], atoms: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(movie(format, atoms)));
    let d = MovDemuxer::open(input).expect("open the movie");
    d.streams()[0].params.extradata.clone()
}

#[test]
fn h264_and_hevc_get_their_configuration_record() {
    assert_eq!(extradata(b"avc1", &[(*b"avcC", &AVCC), (*b"pasp", &PASP), (*b"colr", &COLR)]), AVCC);
    assert_eq!(extradata(b"hvc1", &[(*b"colr", &COLR), (*b"hvcC", &HVCC), (*b"pasp", &PASP)]), HVCC);
}

#[test]
fn mpeg4_video_gets_the_esds_decoder_specific_info() {
    let vol = [0x00, 0x00, 0x01, 0xB0, 0x01, 0x00, 0x00, 0x01, 0xB5, 0x09];
    let mut decoder_config = vec![0x20, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    decoder_config.extend_from_slice(&[0x05, vol.len() as u8]);
    decoder_config.extend_from_slice(&vol);
    let mut es = vec![0x00, 0x01, 0x00, 0x04, decoder_config.len() as u8];
    es.extend_from_slice(&decoder_config);
    es.extend_from_slice(&[0x06, 0x01, 0x02]);
    let mut esds = vec![0, 0, 0, 0, 0x03, es.len() as u8];
    esds.extend_from_slice(&es);
    assert_eq!(extradata(b"mp4v", &[(*b"esds", &esds), (*b"pasp", &PASP)]), vol);
}

#[test]
fn the_first_record_counts_and_a_glbl_wrapping_fiel_is_not_one() {
    // A later `glbl` is ignored once a record is set (FFmpeg: "ignoring
    // multiple glbl"). Old libavformat wrapped a whole `fiel` atom in
    // `glbl`; FFmpeg reads that as atoms, not as a record.
    assert_eq!(extradata(b"avc1", &[(*b"avcC", &AVCC), (*b"glbl", &[9, 9, 9])]), AVCC);
    let mut fiel = Vec::new();
    push_atom(&mut fiel, *b"fiel", &[2, 9]);
    assert_eq!(extradata(b"avc1", &[(*b"glbl", &fiel), (*b"avcC", &AVCC)]), AVCC);
}
