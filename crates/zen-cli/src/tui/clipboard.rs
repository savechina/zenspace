use arboard::Clipboard;

pub fn write_text(text: &str) -> Result<(), String> {
    let mut clip = Clipboard::new().map_err(|e| e.to_string())?;
    clip.set_text(text).map_err(|e| e.to_string())
}
