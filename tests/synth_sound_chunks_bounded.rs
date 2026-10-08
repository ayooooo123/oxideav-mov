//! Grouping QuickTime sound per chunk (`sound_chunks`) stays inside the
//! open-time sample bound: every track's packets are counted against one
//! shared budget before any track's packet list is built. A forged movie
//! whose sound tracks each declare just under the bound, and far over it
//! together, is refused before their packets take any memory.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use oxideav_core::ReadSeek;
use oxideav_mov::MovDemuxer;

/// Live heap bytes of this test binary and their peak.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: each call forwards its arguments unchanged to `System`; the
// counters only record sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A `twos` stereo track whose one chunk declares `samples` 1-byte
/// samples, which FFmpeg's grouping reads as `samples / 1024` packets.
fn sound_trak(track_id: u32, samples: u32) -> Vec<u8> {
    let mut stbl = Vec::new();
    push_atom(&mut stbl, *b"stsd", &build_stsd_audio(b"twos", 2, 16, 44100, &[]));
    push_atom(&mut stbl, *b"stts", &build_stts_single(samples, 1));
    push_atom(&mut stbl, *b"stsc", &build_stsc_single(samples));
    push_atom(&mut stbl, *b"stsz", &build_stsz_constant(1, samples));
    push_atom(&mut stbl, *b"stco", &build_stco_single(0));
    let mut minf = Vec::new();
    push_atom(&mut minf, *b"smhd", &[0u8; 8]);
    push_atom(&mut minf, *b"stbl", &stbl);
    let mut mdia = Vec::new();
    push_atom(&mut mdia, *b"mdhd", &build_mdhd(44100, samples));
    push_atom(&mut mdia, *b"hdlr", &build_hdlr(b"mhlr", b"soun"));
    push_atom(&mut mdia, *b"minf", &minf);
    let mut trak = Vec::new();
    push_atom(&mut trak, *b"tkhd", &build_tkhd(track_id, 0, 0, 0));
    push_atom(&mut trak, *b"mdia", &mdia);
    trak
}

#[test]
fn forged_sound_tracks_are_refused_before_their_packets_are_built() {
    // One track groups to 1 000 000 packets: under the bound's 1 << 20
    // floor on its own. Eight of them are far over it.
    let tracks = 8;
    let mut moov = Vec::new();
    push_atom(&mut moov, *b"mvhd", &build_mvhd(600, 0));
    for id in 1..=tracks {
        push_atom(&mut moov, *b"trak", &sound_trak(id, 1024 * 1_000_000));
    }
    let mut file = Vec::new();
    push_atom(&mut file, *b"moov", &moov);
    let size = file.len();

    let input: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let opened = MovDemuxer::open(input);
    let peak = PEAK.load(Ordering::Relaxed) - base;
    assert!(opened.is_err(), "{tracks} forged tracks in {size} bytes must not open");
    assert!(peak < 16 << 20, "opening {size} bytes peaked at {peak} heap bytes");
}
