//! Response translators: provider format → OpenAI SSE chunks

pub mod claude_to_openai;
pub mod commandcode_to_openai;
pub mod gemini_to_openai;
pub mod non_streaming;
pub mod ollama_to_openai;
pub mod openai_responses;
pub mod openai_to_antigravity;
pub mod openai_to_claude;
pub mod openai_to_gemini;

/// Byte length of the canonical JSON serialization of `value`.
pub(crate) fn serialized_len(value: &serde_json::Value) -> Result<usize, serde_json::Error> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}

#[derive(Default)]
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("serialized length overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
