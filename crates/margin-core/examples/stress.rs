//! Opens a (huge) file and times what the editor does with it: `stress <file> [needle]`.
use std::ops::ControlFlow;
use std::time::Instant;

use margin_core::doc::{Document, Kind, Sel};
use margin_core::{file, rows, search};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&a[1]);
    let needle = a.get(2).cloned().unwrap_or_else(|| "NEEDLE".into());

    let t = Instant::now();
    let o = file::open(path, None).unwrap();
    println!("open {:.1} GB: {:?} ({}, {})", o.size as f64 / 1e9, t.elapsed(), o.encoding.name(), o.eol.name());
    let mut d = Document::new(o.text, Some(path.into()), o.encoding, o.eol);

    // the first screen
    let t = Instant::now();
    let mut s = 0;
    for _ in 0..60 {
        s = rows::next_row(&d.text, s).unwrap_or(s);
    }
    println!("first 60 rows: {:?}", t.elapsed());

    // jump to the middle and to the end, before any indexing finished
    let t = Instant::now();
    let mid = rows::row_start(&d.text, d.len() / 2);
    let end = rows::row_start(&d.text, d.len());
    let mut r = end;
    for _ in 0..60 {
        r = rows::prev_row(&d.text, r).unwrap_or(r);
    }
    println!("jump to middle ({mid}) and end, 60 rows up: {:?}", t.elapsed());
    println!("line of middle while indexing: {:?} ({:.0}% indexed)", d.text.line_of(mid), d.text.index_progress() * 100.0);

    // the 50 MB line comes in rows of a few KB
    let long = d.len() - 50_000_000 / 2;
    let rs = rows::row_start(&d.text, long);
    let (te, next) = rows::row_end(&d.text, rs);
    println!("row inside the 50 MB line: {rs}..{te} (next {next:?})");

    // indexing
    let t = Instant::now();
    while !d.text.indexed() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("indexed in {:?} (+ open); lines: {:?}", t.elapsed(), d.text.line_count());
    let t = Instant::now();
    let n = d.text.line_count().unwrap();
    let target = n / 3;
    let at = d.text.line_start(target).unwrap();
    assert_eq!(d.text.line_of(at), Some(target));
    println!("go to line {target}: offset {at}, {:?}", t.elapsed());

    // find from the end backwards, and forwards from the middle
    let re = search::Query { pattern: needle.clone(), ..Default::default() }.compile().unwrap();
    let p = search::Progress::default();
    let t = Instant::now();
    let back = search::find_prev(&d.text, &re, d.len(), false, &p);
    println!("find '{needle}' backwards from the end: {back:?} in {:?}", t.elapsed());
    let t = Instant::now();
    let fwd = search::find_next(&d.text, &re, d.len() / 2, false, &p);
    println!("find '{needle}' forwards from the middle (scans ~10 GB): {fwd:?} in {:?}", t.elapsed());

    // edit in the middle, undo, redo
    let t = Instant::now();
    d.sel = Sel::caret(mid);
    d.insert(b"EDITED IN THE MIDDLE\n", Kind::Other);
    d.sel = Sel::caret(0);
    d.insert(b"first line\n", Kind::Other);
    println!("two edits: {:?}; lines now {:?}", t.elapsed(), d.text.line_count());
    assert_eq!(&d.text.read(mid + 11..mid + 31), b"EDITED IN THE MIDDLE");

    if a.iter().any(|x| x == "--save") {
        let out = path.with_extension("saved");
        let t = Instant::now();
        file::save(&d.text, &out, d.encoding, &file::Progress::default()).unwrap();
        let size = std::fs::metadata(&out).unwrap().len();
        println!("saved {:.1} GB in {:?}", size as f64 / 1e9, t.elapsed());
        assert_eq!(size, d.len());
        // spot-check
        let o = file::open(&out, None).unwrap();
        assert_eq!(o.text.read(0..11), b"first line\n");
        assert_eq!(o.text.read(mid + 11..mid + 31), b"EDITED IN THE MIDDLE");
        std::fs::remove_file(out).unwrap();
        println!("saved file checks out");
    }
    let _ = d.text.chunks::<()>(0..1, |_, _| ControlFlow::Continue(()));
}
