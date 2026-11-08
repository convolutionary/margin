//! Opening and saving files: encodings, line endings, and doing both without loading big files.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::index::LineIndex;
use crate::text::{Bytes, INDEX_MIN, Source, Text};

/// Files up to this size are read into memory; bigger ones are memory-mapped.
pub const MAP_MIN: u64 = 64 << 20;
/// Files that need converting (UTF-16, legacy code pages) are converted in memory, up to this size.
pub const CONVERT_MAX: u64 = 2 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// an ASCII-compatible legacy encoding, converted to UTF-8 while open
    Legacy(&'static encoding_rs::Encoding),
}

impl Encoding {
    pub fn name(&self) -> String {
        match self {
            Encoding::Utf8 => "UTF-8".into(),
            Encoding::Utf8Bom => "UTF-8 with BOM".into(),
            Encoding::Utf16Le => "UTF-16 LE".into(),
            Encoding::Utf16Be => "UTF-16 BE".into(),
            Encoding::Legacy(e) => e.name().into(),
        }
    }

    /// Whether saving needs the whole text converted (so it's only offered for files that fit in memory).
    pub fn converts(&self) -> bool {
        !matches!(self, Encoding::Utf8 | Encoding::Utf8Bom)
    }

    /// Legacy encodings offered in the menu, by name.
    pub fn legacy_choices() -> Vec<&'static encoding_rs::Encoding> {
        use encoding_rs::*;
        vec![WINDOWS_1252, ISO_8859_2, ISO_8859_15, WINDOWS_1250, WINDOWS_1251, KOI8_R, SHIFT_JIS, EUC_JP, GBK, BIG5, EUC_KR]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::CrLf => b"\r\n",
            Eol::Cr => b"\r",
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::Cr => "CR",
        }
    }
    pub fn native() -> Eol {
        if cfg!(windows) { Eol::CrLf } else { Eol::Lf }
    }
}

pub struct Opened {
    pub text: Text,
    pub encoding: Encoding,
    pub eol: Eol,
    /// NUL bytes near the start: probably not text, shown anyway
    pub binary: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// The first line ending in `sample`, or the platform's.
pub fn detect_eol(sample: &[u8]) -> Eol {
    match memchr::memchr2(b'\n', b'\r', sample) {
        Some(i) if sample[i] == b'\r' => {
            if sample.get(i + 1) == Some(&b'\n') { Eol::CrLf } else { Eol::Cr }
        }
        Some(_) => Eol::Lf,
        None => Eol::native(),
    }
}

/// Valid UTF-8, allowing a character cut off at the end of the sample.
fn utf8ish(sample: &[u8]) -> bool {
    match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && sample.len() - e.valid_up_to() < 4,
    }
}

fn source_of(bytes: Bytes, path: Option<&Path>) -> Arc<Source> {
    let len = bytes.len() as u64;
    if len < INDEX_MIN {
        return Arc::new(Source { bytes, index: None });
    }
    if len <= MAP_MIN || path.is_none() {
        // in memory already: index now, it takes milliseconds
        let ix = Arc::new(LineIndex::build(&bytes));
        return Arc::new(Source { bytes, index: Some(ix) });
    }
    // mapped: index on a thread from its own read-only map of the same file
    let ix = LineIndex::pending(len);
    let p = path.unwrap().to_path_buf();
    LineIndex::background(
        len,
        move || {
            let f = File::open(&p).ok()?;
            let m = unsafe { memmap2::Mmap::map(&f) }.ok()?;
            #[cfg(unix)]
            let _ = m.advise(memmap2::Advice::Sequential);
            (m.len() as u64 >= len).then_some(m)
        },
        ix.clone(),
    );
    Arc::new(Source { bytes, index: Some(ix) })
}

/// Opens a file. Big files are mapped, not read; a forced encoding skips detection.
pub fn open(path: &Path, force: Option<Encoding>) -> io::Result<Opened> {
    let f = File::open(path)?;
    let meta = f.metadata()?;
    if meta.is_dir() {
        return Err(io::Error::other("that's a folder"));
    }
    let size = meta.len();
    let bytes = if size == 0 {
        Bytes::Owned(Vec::new())
    } else if size <= MAP_MIN {
        Bytes::Owned(std::fs::read(path)?)
    } else {
        Bytes::Map(unsafe { memmap2::Mmap::map(&f)? })
    };
    let head = &bytes[..bytes.len().min(64 << 10)];
    let binary = memchr::memchr(0, &head[..head.len().min(8 << 10)]).is_some()
        && !head.starts_with(&[0xFF, 0xFE])
        && !head.starts_with(&[0xFE, 0xFF]);

    let encoding = force.unwrap_or_else(|| {
        if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
            Encoding::Utf8Bom
        } else if head.starts_with(&[0xFF, 0xFE]) {
            Encoding::Utf16Le
        } else if head.starts_with(&[0xFE, 0xFF]) {
            Encoding::Utf16Be
        } else if binary || utf8ish(head) || size > CONVERT_MAX {
            // too big to convert: shown as UTF-8, odd bytes as replacement characters
            Encoding::Utf8
        } else {
            let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            det.feed(head, head.len() == bytes.len());
            let e = det.guess(None, chardetng::Utf8Detection::Allow);
            if e == encoding_rs::UTF_8 { Encoding::Utf8 } else { Encoding::Legacy(e) }
        }
    });

    let (text, eol) = match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => {
            let skip = if encoding == Encoding::Utf8Bom && bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
            let eol = detect_eol(&head[skip.min(head.len())..]);
            let len = bytes.len() as u64;
            (Text::from_source(source_of(bytes, Some(path)), skip as u64..len), eol)
        }
        _ => {
            if size > CONVERT_MAX {
                return Err(io::Error::other(format!("{} files over 2 GB can't be converted; open it as UTF-8", encoding.name())));
            }
            let utf8 = decode(&bytes, encoding);
            let eol = detect_eol(&utf8[..utf8.len().min(64 << 10)]);
            let n = utf8.len() as u64;
            (Text::from_source(source_of(Bytes::Owned(utf8), None), 0..n), eol)
        }
    };
    Ok(Opened { text, encoding, eol, binary, size, modified: meta.modified().ok() })
}

/// Bytes in `enc` to UTF-8.
pub fn decode(bytes: &[u8], enc: Encoding) -> Vec<u8> {
    let e = match enc {
        Encoding::Utf8 | Encoding::Utf8Bom => encoding_rs::UTF_8,
        Encoding::Utf16Le => encoding_rs::UTF_16LE,
        Encoding::Utf16Be => encoding_rs::UTF_16BE,
        Encoding::Legacy(e) => e,
    };
    // decode() strips a BOM matching the encoding
    e.decode(bytes).0.into_owned().into_bytes()
}

/// Progress of a long save, readable from the UI thread.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 { 0.0 } else { self.done.load(Ordering::Relaxed) as f32 / t as f32 }
    }
}

/// Where a save is written before it replaces the file, next to it so the rename is atomic.
fn temp_beside(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.margin-{}", std::process::id()))
}

/// Writes `text` to `path` in `enc`: into a temporary file beside it, then renamed over it, so a
/// failed or cancelled save never leaves half a file. Keeps the old file's permissions.
pub fn save(text: &Text, path: &Path, enc: Encoding, progress: &Progress) -> io::Result<()> {
    let tmp = temp_beside(path);
    let res = (|| {
        let f = File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(8 << 20, f);
        write(text, &mut w, enc, progress)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Writes `text` in `enc`, honouring `progress.cancel`.
pub fn write(text: &Text, w: &mut impl Write, enc: Encoding, progress: &Progress) -> io::Result<()> {
    progress.total.store(text.len(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    match enc {
        Encoding::Utf8Bom => w.write_all(&[0xEF, 0xBB, 0xBF])?,
        Encoding::Utf16Le => w.write_all(&[0xFF, 0xFE])?,
        Encoding::Utf16Be => w.write_all(&[0xFE, 0xFF])?,
        _ => {}
    }
    let mut encoder = match enc {
        Encoding::Legacy(e) => Some(e.new_encoder()),
        _ => None,
    };
    let mut carry: Vec<u8> = Vec::new(); // a UTF-8 character split across chunks
    let mut out = Vec::new();
    let res = text.chunks(0..text.len(), |at, chunk| {
        if progress.cancel.load(Ordering::Relaxed) {
            return ControlFlow::Break(Err::<(), io::Error>(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        }
        let r = match enc {
            Encoding::Utf8 | Encoding::Utf8Bom => w.write_all(chunk),
            _ => {
                carry.extend_from_slice(chunk);
                let valid = match std::str::from_utf8(&carry) {
                    Ok(_) => carry.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(e) => e.valid_up_to() + e.error_len().unwrap_or(1),
                };
                let s = String::from_utf8_lossy(&carry[..valid]).into_owned();
                carry.drain(..valid);
                out.clear();
                match enc {
                    Encoding::Utf16Le => s.encode_utf16().for_each(|u| out.extend_from_slice(&u.to_le_bytes())),
                    Encoding::Utf16Be => s.encode_utf16().for_each(|u| out.extend_from_slice(&u.to_be_bytes())),
                    _ => {
                        let e = encoder.as_mut().unwrap();
                        out.reserve(e.max_buffer_length_from_utf8_if_no_unmappables(s.len()).unwrap_or(s.len() * 4) + 16);
                        let (_, _, _) = e.encode_from_utf8_to_vec(&s, &mut out, false);
                    }
                }
                w.write_all(&out)
            }
        };
        progress.done.store(at + chunk.len() as u64, Ordering::Relaxed);
        match r {
            Ok(()) => ControlFlow::Continue(()),
            Err(e) => ControlFlow::Break(Err(e)),
        }
    });
    if let Some(Err(e)) = res {
        return Err(e);
    }
    if let Some(e) = encoder.as_mut() {
        out.clear();
        out.reserve(64);
        let _ = e.encode_from_utf8_to_vec("", &mut out, true);
        w.write_all(&out)?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        for (enc, sample) in [
            (Encoding::Utf8, "héllo\r\nwörld\r\n"),
            (Encoding::Utf8Bom, "bom ✓\n"),
            (Encoding::Utf16Le, "utf16 ✓ 𝄞\n"),
            (Encoding::Utf16Be, "be\n"),
            (Encoding::Legacy(encoding_rs::WINDOWS_1252), "café crème\n"),
        ] {
            let t = Text::from_bytes(sample.as_bytes().to_vec());
            save(&t, &p, enc, &Progress::default()).unwrap();
            let o = open(&p, None).unwrap();
            assert_eq!(o.encoding, enc, "{sample}");
            assert_eq!(String::from_utf8(o.text.read(0..o.text.len())).unwrap(), sample);
        }
        let o = open(&p, None).unwrap();
        assert_eq!(o.eol, Eol::Lf);
        std::fs::write(&p, b"a\r\nb").unwrap();
        assert_eq!(open(&p, None).unwrap().eol, Eol::CrLf);
    }
}
