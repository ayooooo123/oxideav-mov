//! QuickTime `text` and 3GPP `tx3g` tracks are subtitle streams. As in
//! FFmpeg 2da55bf: both sample-entry formats are `mov_text`
//! (`libavformat/isom.c` `ff_codec_movsubtitle_tags`), the extradata is
//! the entry after its universal 16-byte header (`mov_parse_stsd_subtitle`),
//! and a text track that a `tref/chap` points at holds chapter titles, not
//! subtitles: FFmpeg's `mov_read_chapters` makes it a data stream.

#![cfg(feature = "registry")]

mod common;

use std::io::Cursor;

use common::*;
use oxideav_core::{Demuxer, MediaType, ReadSeek};
use oxideav_mov::MovDemuxer;

/// The sample-entry body FFmpeg's `mov_text` encoder writes
/// (`movtextenc.c` `text_sample_entry`): display flags, justification,
/// background colour, default text box, style record and a one-font
/// `ftab`. FFmpeg's MOV muxer writes it under `text` as well as `tx3g`.
const ENTRY_BODY: [u8; 48] = [
    0, 0, 0, 0, 0x01, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0x12, 0xFF, 0xFF, 0xFF,
    0xFF, 0, 0, 0, 0x12, b'f', b't', b'a', b'b', 0, 0x01, 0, 0x01, 0x05, b'S', b'e', b'r', b'i', b'f',
];

fn stsd(format: &[u8; 4]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    p.extend_from_slice(&1u32.to_be_bytes()); // entry count
    p.extend_from_slice(&(16 + ENTRY_BODY.len() as u32).to_be_bytes());
    p.extend_from_slice(format);
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data reference index
    p.extend_from_slice(&ENTRY_BODY);
    p
}

/// `[u16 length][text]`, the sample format of both entries.
fn sample(text: &str) -> Vec<u8> {
    let mut v = (text.len() as u16).to_be_bytes().to_vec();
    v.extend_from_slice(text.as_bytes());
    v
}

/// A one-sample text-handler track at `offset`.
fn text_trak(id: u32, format: &[u8; 4], sample_len: u32, offset: u32, chapters: Option<u32>) -> Vec<u8> {
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(id, 600, 0, 0));
    if let Some(chapter_id) = chapters {
        let mut tref = Vec::new();
        push_atom(&mut tref, *b"chap", &chapter_id.to_be_bytes());
        push_atom(&mut trak, *b"tref", &tref);
    }
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(1000, 1000));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"text"));
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &stsd(format));
    push_atom(&mut stbl, *b"stts", &build_stts_single(1, 1000));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(1));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(sample_len, 1));
    push_atom(&mut stbl, *b"stco", &build_stco_single(offset));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"gmhd", &[]);
    push_atom(&mut minf, *b"stbl", &stbl);
    push_atom(&mut mdia, *b"minf", &minf);
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

/// Track 1: `text` subtitles, its `tref/chap` naming track 3. Track 2:
/// `tx3g` subtitles. Track 3: the `text` chapter list.
fn movie() -> Vec<u8> {
    let mut out = Vec::new();
    push_atom(&mut out, *b"ftyp", &[b"qt  ".as_slice(), &0u32.to_be_bytes(), b"qt  "].concat());
    let samples = [sample("Hello"), sample("World"), sample("Chapter")];
    let mut offset = (out.len() + 8) as u32;
    let mut offsets = Vec::new();
    for s in &samples {
        offsets.push(offset);
        offset += s.len() as u32;
    }
    push_atom(&mut out, *b"mdat", &samples.concat());
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 600));
    push_atom(&mut moov, *b"trak", &text_trak(1, b"text", samples[0].len() as u32, offsets[0], Some(3)));
    push_atom(&mut moov, *b"trak", &text_trak(2, b"tx3g", samples[1].len() as u32, offsets[1], None));
    push_atom(&mut moov, *b"trak", &text_trak(3, b"text", samples[2].len() as u32, offsets[2], None));
    push_atom(&mut out, *b"moov", &moov);
    out
}

#[test]
fn text_and_tx3g_tracks_are_mov_text_subtitles_and_chapters_stay_data() {
    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(movie()));
    let mut d = MovDemuxer::open(input).expect("open text-track fixture");
    let streams = d.streams().to_vec();
    assert_eq!(streams.len(), 3);
    for s in &streams[..2] {
        assert_eq!(s.params.media_type, MediaType::Subtitle, "stream {}", s.index);
        assert_eq!(s.params.codec_id.as_str(), "mov_text", "stream {}", s.index);
        assert_eq!(s.params.extradata, ENTRY_BODY, "stream {}", s.index);
    }
    assert_eq!(streams[2].params.media_type, MediaType::Data, "the chapter track");

    let mut texts = Vec::new();
    while let Ok(p) = d.next_packet() {
        texts.push((p.stream_index, p.data.clone()));
    }
    texts.sort();
    assert_eq!(texts, [(0, sample("Hello")), (1, sample("World")), (2, sample("Chapter"))]);
}
