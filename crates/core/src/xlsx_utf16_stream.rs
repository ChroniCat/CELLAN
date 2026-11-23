//! Decode UTF-16 worksheet or shared-string parts without buffering a complete ZIP entry.
use anyhow::{Context, Result};
use encoding_rs::{Decoder, DecoderResult, Encoding};
use std::io::{self, BufRead, BufReader, Cursor, Read};

struct Utf8<R> {
    source: R,
    decoder: Decoder,
    decoded: [u8; 8192],
    available: std::ops::Range<usize>,
    finished: bool,
}

impl<R: BufRead> Read for Utf8<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.available.is_empty() && !self.finished {
            let input = self.source.fill_buf()?;
            let last = input.is_empty();
            let (status, used, written) =
                self.decoder
                    .decode_to_utf8_without_replacement(input, &mut self.decoded, last);
            self.source.consume(used);
            if matches!(status, DecoderResult::Malformed(..)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid UTF-16 XML",
                ));
            }
            self.available = 0..written;
            self.finished = last && status == DecoderResult::InputEmpty;
        }
        let count = output.len().min(self.available.len());
        output[..count]
            .copy_from_slice(&self.decoded[self.available.start..self.available.start + count]);
        self.available.start += count;
        Ok(count)
    }
}

pub(super) fn process<T>(
    source: impl BufRead,
    encoding: &'static Encoding,
    part: &str,
    process: impl FnOnce(&mut dyn Read) -> Result<T>,
) -> Result<T> {
    let mut input = BufReader::new(Utf8 {
        source,
        decoder: encoding.new_decoder_with_bom_removal(),
        decoded: [0; 8192],
        available: 0..0,
        finished: false,
    });
    let mut head = Vec::new();
    input.by_ref().take(6).read_to_end(&mut head)?;
    if head.starts_with(b"<?xml") && head.get(5).is_some_and(u8::is_ascii_whitespace) {
        input.read_until(b'>', &mut head)?;
        let mut reader = quick_xml::Reader::from_reader(head.as_slice());
        let quick_xml::events::Event::Decl(decl) = reader.read_event()? else {
            anyhow::bail!("{part}: invalid XML declaration");
        };
        let version = decl.version()?;
        let standalone = decl.standalone().transpose()?;
        let decl =
            quick_xml::events::BytesDecl::new(&version, Some("UTF-8"), standalone.as_deref());
        let mut writer = quick_xml::Writer::new(Vec::new());
        writer.write_event(quick_xml::events::Event::Decl(decl))?;
        head = writer.into_inner();
    }
    process(&mut Cursor::new(head).chain(input)).with_context(|| format!("processing {part}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_surrogates_and_declarations_transcode_for_either_byte_order() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16" standalone="yes"?><row>😀漢</row>"#;
        for big in [true, false] {
            let encoding = if big {
                encoding_rs::UTF_16BE
            } else {
                encoding_rs::UTF_16LE
            };
            let mut bytes = vec![];
            for c in "\u{feff}".encode_utf16().chain(xml.encode_utf16()) {
                bytes.extend(if big {
                    c.to_be_bytes()
                } else {
                    c.to_le_bytes()
                });
            }
            process(
                BufReader::with_capacity(1, bytes.as_slice()),
                encoding,
                "sheet",
                |r| {
                    let mut decoded = String::new();
                    r.read_to_string(&mut decoded)?;
                    assert_eq!(decoded, xml.replace("UTF-16", "UTF-8"));
                    Ok(true)
                },
            )
            .unwrap();
            bytes.pop();
            assert!(
                process(
                    BufReader::with_capacity(3, bytes.as_slice()),
                    encoding,
                    "sheet",
                    |r| {
                        io::copy(r, &mut io::sink())?;
                        Ok(true)
                    }
                )
                .is_err()
            );
        }
    }
}
