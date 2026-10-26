//! 剪贴板读写（arboard）。

pub fn copy_text(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut c) => c.set_text(text).is_ok(),
        Err(_) => false,
    }
}

pub fn read_text() -> Option<String> {
    let mut c = arboard::Clipboard::new().ok()?;
    c.get_text().ok().filter(|s| !s.is_empty())
}
