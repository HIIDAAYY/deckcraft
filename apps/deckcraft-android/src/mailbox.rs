//! Messages the Java side of the app hands to Rust.
//!
//! The Android pickers, "Open with DeckCraft", the save picker and the copy back to a presentation's
//! own file all answer on Java's UI thread or a worker, long after the Rust call that started them
//! returned. Rather than calling back into Rust across threads, the activity drops small files into
//! the *inbox* folder, each written under a `.tmp` name and renamed when complete, and the app
//! drains them at the start of a frame. Names sort in the order they were written.
//!
//! | file | content (lines) |
//! |---|---|
//! | `<seq>.open` | a picked or shared file: its display name, what it's for (`open`, `picture`, `audio`, `video`, `theme`), its `content://` address when the app may write back to it (else empty), the request id the app gave the picker (0 for a file opened from another app), then its bytes |
//! | `<seq>.done` | a picker closed (whether or not it delivered a file): what it was for, and its request id |
//! | `<seq>.save` | the save picker answered: the save's ticket; `ok`, `cancel` or `fail`; the name the app asked for; then for `ok` the name the user chose and its address, for `fail` the reason |
//! | `<seq>.sync` | a presentation's working copy was copied back to its own file: the copy's path; `ok`, `conflict` (the file changed elsewhere since, so it was left alone) or `fail`; the reason |
//! | `<seq>.text` | text the on-screen keyboard typed (letters, accents, emoji, a word from suggestions) |
//! | `<seq>.key` | a key the on-screen keyboard pressed that isn't text (Backspace, Enter, arrows, Ctrl+…): the Android key code, a newline, its meta state |
//! | `<seq>.insets` | the window area covered by the status bar, navigation bar, notch or keyboard, in pixels: `left top right bottom` |
//!
//! The keyboard's text and keys share the inbox so they reach the app in the order typed.
//!
//! Pure file handling with no Android dependency, so it is tested on the desktop.

use std::path::Path;

/// A file the app reads is capped at this size (a presentation, a picture or a video).
pub const MAX_FILE_BYTES: u64 = 1 << 30;

/// One message from the Java side.
#[derive(Debug, PartialEq)]
pub enum Message {
    /// A file to open or insert, for the picker request `id` (0: opened from another app).
    /// `purpose`: what the picker was for. `uri`: its address, when the app may write back to it.
    Opened { name: String, purpose: String, uri: Option<String>, id: u64, bytes: Vec<u8> },
    /// The picker for request `id` closed.
    PickerClosed { purpose: String, id: u64 },
    /// The save picker answered for save `ticket`, which the app asked to save as `requested`.
    Saved { ticket: u64, requested: String, outcome: Outcome },
    /// A working copy was copied back (or not) to its presentation's own file.
    Synced { path: String, outcome: Sync },
    /// Typed text.
    Text(String),
    /// A key from the on-screen keyboard: Android's key code and meta state.
    Key { code: i32, meta: i32 },
    /// The edges of the window covered by system bars or the keyboard, in pixels.
    Insets { left: f32, top: f32, right: f32, bottom: f32 },
}

/// How the save picker ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Written, under the name the user chose, at that address.
    Saved {
        name: String,
        uri: String,
    },
    Cancelled,
    Failed(String),
}

/// How a copy back to a presentation's own file ended.
#[derive(Debug, PartialEq)]
pub enum Sync {
    Written,
    /// The file changed elsewhere since DeckCraft last read or wrote it: left alone.
    Conflict,
    Failed(String),
}

/// Read and remove every complete message in `dir`, oldest first. A message that can't be read
/// is logged and removed, so it never blocks the ones after it.
pub fn drain(dir: &Path) -> Vec<Message> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e != "tmp")).collect();
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let kind = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_string();
        let too_big = std::fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES.saturating_add(8192)).unwrap_or(true);
        let parsed = if too_big {
            Err("too large".to_string())
        } else {
            std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| parse(&kind, bytes))
        };
        match parsed {
            Ok(Some(m)) => out.push(m),
            Ok(None) => {}
            Err(e) => log::warn!("inbox: skipping {}: {e}", path.display()),
        }
        if let Err(e) = std::fs::remove_file(&path) {
            log::warn!("inbox: cannot remove {}: {e}", path.display());
        }
    }
    out
}

/// Whether `dir` holds a complete message (for the thread that wakes the app when one arrives).
pub fn has_mail(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x != "tmp"))))
}

/// The first `n` lines of `bytes` (as text) and the bytes after them.
fn header(bytes: &[u8], n: usize) -> Option<(Vec<String>, &[u8])> {
    let mut lines = Vec::with_capacity(n);
    let mut rest = bytes;
    for _ in 0..n {
        let split = rest.iter().position(|&b| b == b'\n')?;
        lines.push(String::from_utf8_lossy(rest.get(..split)?).trim().to_string());
        rest = rest.get(split.saturating_add(1)..)?;
    }
    Some((lines, rest))
}

/// Line `i` of `lines`, empty when missing.
fn line(lines: &[&str], i: usize) -> String {
    lines.get(i).map(|l| l.trim().to_string()).unwrap_or_default()
}

/// One message from its kind (the file extension) and content. `Ok(None)` for a kind this
/// version doesn't know (a newer Java side), which is skipped.
pub fn parse(kind: &str, bytes: Vec<u8>) -> Result<Option<Message>, String> {
    match kind {
        "open" => {
            let (head, data) = header(&bytes, 4).ok_or("no header")?;
            let name = safe_name(head.first().map(String::as_str).unwrap_or_default());
            let purpose = head.get(1).cloned().unwrap_or_default();
            let uri = head.get(2).filter(|u| !u.is_empty()).cloned();
            let id = head.get(3).and_then(|i| i.parse().ok()).unwrap_or(0);
            Ok(Some(Message::Opened { name, purpose, uri, id, bytes: data.to_vec() }))
        }
        "done" => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.split('\n').collect();
            Ok(Some(Message::PickerClosed { purpose: line(&lines, 0), id: line(&lines, 1).parse().unwrap_or(0) }))
        }
        "save" => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.split('\n').collect();
            let ticket = line(&lines, 0).parse::<u64>().map_err(|_| "a save with no ticket")?;
            let requested = safe_name(&line(&lines, 2));
            let outcome = match line(&lines, 1).as_str() {
                "ok" => {
                    let uri = line(&lines, 4);
                    if uri.is_empty() {
                        return Err("a save with no address".into());
                    }
                    Outcome::Saved { name: safe_name(&line(&lines, 3)), uri }
                }
                "cancel" => Outcome::Cancelled,
                "fail" => Outcome::Failed(Some(line(&lines, 3)).filter(|r| !r.is_empty()).unwrap_or_else(|| "unknown error".into())),
                other => return Err(format!("unknown save outcome {other:?}")),
            };
            Ok(Some(Message::Saved { ticket, requested, outcome }))
        }
        "sync" => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.split('\n').collect();
            let path = line(&lines, 0);
            if path.is_empty() {
                return Err("a sync with no path".into());
            }
            let outcome = match line(&lines, 1).as_str() {
                "ok" => Sync::Written,
                "conflict" => Sync::Conflict,
                "fail" => Sync::Failed(Some(line(&lines, 2)).filter(|r| !r.is_empty()).unwrap_or_else(|| "unknown error".into())),
                other => return Err(format!("unknown sync outcome {other:?}")),
            };
            Ok(Some(Message::Synced { path, outcome }))
        }
        "text" => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            Ok((!text.is_empty()).then_some(Message::Text(text)))
        }
        "key" => {
            let text = String::from_utf8_lossy(&bytes);
            let mut parts = text.split_whitespace().map(str::parse::<i32>);
            let code = parts.next().and_then(Result::ok).ok_or("no key code")?;
            let meta = parts.next().and_then(Result::ok).unwrap_or(0);
            Ok(Some(Message::Key { code, meta }))
        }
        "insets" => {
            let text = String::from_utf8_lossy(&bytes);
            let v: Vec<f32> =
                text.split_whitespace().filter_map(|n| n.parse::<f32>().ok()).filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 10_000.0)).collect();
            match v.as_slice() {
                [left, top, right, bottom] => Ok(Some(Message::Insets { left: *left, top: *top, right: *right, bottom: *bottom })),
                _ => Err(format!("insets need four numbers, got {text:?}")),
            }
        }
        _ => Ok(None),
    }
}

/// A file name safe to show and to use as one path component: no folders, no control
/// characters, not empty, at most 200 characters.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "Presentation.pptx".to_string() } else { clean }
}

/// Whether `name` is a presentation DeckCraft saves in full as its own file (the others are
/// copies: a PDF, pictures, an outline).
pub fn is_presentation_save(name: &str) -> bool {
    matches!(name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("pptx" | "deckcraft" | "ppsx" | "potx"))
}

/// The MIME type Android's save picker should offer for `name`.
pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("ppsx") => "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
        Some("potx") => "application/vnd.openxmlformats-officedocument.presentationml.template",
        Some("deckcraft") => "application/zip",
        Some("txt") => "text/plain",
        Some("rtf") => "application/rtf",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_files_carry_purpose_address_request_and_bytes() {
        let mut bytes = b"../x/Talk.pptx\nopen\ncontent://docs/1\n7\n".to_vec();
        bytes.extend([0u8, 10, 13, 255]);
        assert_eq!(
            parse("open", bytes),
            Ok(Some(Message::Opened {
                name: "Talk.pptx".into(),
                purpose: "open".into(),
                uri: Some("content://docs/1".into()),
                id: 7,
                bytes: vec![0, 10, 13, 255]
            }))
        );
        let pic = parse("open", b"logo.png\npicture\n\n3\nPNG".to_vec());
        assert_eq!(pic, Ok(Some(Message::Opened { name: "logo.png".into(), purpose: "picture".into(), uri: None, id: 3, bytes: b"PNG".to_vec() })));
        let shared = parse("open", b"a.pptx\nopen\n\n\nPK".to_vec());
        assert!(matches!(shared, Ok(Some(Message::Opened { id: 0, .. }))), "no request: from another app");
        assert!(parse("open", b"only a name\n".to_vec()).is_err());
    }

    #[test]
    fn save_answers() {
        assert_eq!(
            parse("save", b"7\nok\nBook1.xlsx\nBudget 2026.xlsx\ncontent://docs/9".to_vec()),
            Ok(Some(Message::Saved {
                ticket: 7,
                requested: "Book1.xlsx".into(),
                outcome: Outcome::Saved { name: "Budget 2026.xlsx".into(), uri: "content://docs/9".into() }
            }))
        );
        assert_eq!(
            parse("save", b"8\ncancel\nBook1.xlsx".to_vec()),
            Ok(Some(Message::Saved { ticket: 8, requested: "Book1.xlsx".into(), outcome: Outcome::Cancelled }))
        );
        assert_eq!(
            parse("save", b"9\nfail\nBook1.csv\ndisk full".to_vec()),
            Ok(Some(Message::Saved { ticket: 9, requested: "Book1.csv".into(), outcome: Outcome::Failed("disk full".into()) }))
        );
        assert!(parse("save", b"1\nok\nBook1.xlsx\nB.xlsx\n".to_vec()).is_err(), "a save needs its address");
        assert!(parse("save", b"1\nmaybe\nBook1.xlsx".to_vec()).is_err());
        assert!(parse("save", b"x\ncancel\nBook1.xlsx".to_vec()).is_err(), "a save needs its ticket");
    }

    #[test]
    fn sync_answers() {
        assert_eq!(parse("sync", b"/w/1/B.xlsx\nok".to_vec()), Ok(Some(Message::Synced { path: "/w/1/B.xlsx".into(), outcome: Sync::Written })));
        assert_eq!(
            parse("sync", b"/w/1/B.xlsx\nconflict\n".to_vec()),
            Ok(Some(Message::Synced { path: "/w/1/B.xlsx".into(), outcome: Sync::Conflict }))
        );
        assert_eq!(
            parse("sync", b"/w/1/B.xlsx\nfail\nno permission".to_vec()),
            Ok(Some(Message::Synced { path: "/w/1/B.xlsx".into(), outcome: Sync::Failed("no permission".into()) }))
        );
        assert!(parse("sync", b"\nok".to_vec()).is_err());
    }

    #[test]
    fn keys_insets_and_unknown_kinds() {
        assert_eq!(parse("key", b"67\n0".to_vec()), Ok(Some(Message::Key { code: 67, meta: 0 })));
        assert_eq!(parse("insets", b"0 72 0 48".to_vec()), Ok(Some(Message::Insets { left: 0.0, top: 72.0, right: 0.0, bottom: 48.0 })));
        assert!(parse("insets", b"1 2 3".to_vec()).is_err());
        assert_eq!(parse("future", vec![1, 2]), Ok(None));
        assert_eq!(parse("text", Vec::new()), Ok(None));
        assert_eq!(parse("done", b"picture\n4".to_vec()), Ok(Some(Message::PickerClosed { purpose: "picture".into(), id: 4 })));
    }

    #[test]
    fn names_and_kinds() {
        assert_eq!(safe_name("a/b\\c.xlsx"), "c.xlsx");
        assert_eq!(safe_name("..\u{0}"), "Presentation.pptx");
        assert!(is_presentation_save("Talk.PPTX"));
        assert!(is_presentation_save("Talk.deckcraft"));
        assert!(!is_presentation_save("Talk.pdf"));
        assert_eq!(mime_for("x.pptx"), "application/vnd.openxmlformats-officedocument.presentationml.presentation");
    }

    #[test]
    fn drain_reads_in_order_skips_partial_files_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("deckcraft-android-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0002.text"), "b").unwrap();
        std::fs::write(dir.join("0001.text"), "a").unwrap();
        std::fs::write(dir.join("0003.open.tmp"), "half written").unwrap();
        std::fs::write(dir.join("0004.save"), "nonsense").unwrap();
        assert!(has_mail(&dir));
        assert_eq!(drain(&dir), vec![Message::Text("a".into()), Message::Text("b".into())]);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, vec!["0003.open.tmp".to_string()], "the bad message is gone, the partial one waits");
        assert!(!has_mail(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
