//! Hostile files claiming to be JPEG, PNG, GIF, ZIP, PDF or MP4: checks must terminate without
//! panicking or exhausting memory.
#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for name in ["a.jpg", "a.png", "a.gif", "a.docx", "a.pdf", "a.mp4", "a.bin"] {
        let _ = rescyo_verify::verify(&mut Cursor::new(data), data.len() as u64, name);
    }
});
