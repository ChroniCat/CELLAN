//! Shared strings are decoded one item at a time. A worksheet row (including its
//! closing tag) always belongs to one worker. Commit results in original order.
use super::{Formulas, Loaded, RowCells, Strings, attr, expand_shared, parse_section, string_item};
use crate::xlsx::delta::stream::Window;
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, events::Event};
use std::io::{self, Read};

pub(super) fn string_table(input: impl Read, table: &mut Strings) -> Result<()> {
    let mut reader = Window::start(input, &mut io::sink())?;
    let (mut token, mut skip) = (Vec::new(), Vec::new());
    let (mut opened, mut closed) = (false, false);
    loop {
        token.clear();
        let event = reader.read_event_into(&mut token)?;
        let mut item = false;
        match event {
            Event::Start(ref t) | Event::Empty(ref t) => {
                let empty = matches!(event, Event::Empty(_));
                if !opened {
                    ensure!(t.local_name().as_ref() == "sst", "无效共享字符串根元素");
                    opened = true;
                    closed = empty;
                } else {
                    ensure!(!closed, "共享字符串根元素重复");
                    item = t.local_name().as_ref() == "si";
                    if !empty {
                        skip.clear();
                        reader.read_to_end_into(t.name(), &mut skip)?;
                    }
                }
            }
            Event::End(_) => closed = true,
            Event::Eof => break,
            _ => {}
        }
        let end = reader.buffer_position();
        let xml = std::str::from_utf8(reader.get_mut().through(end)?)?;
        if item {
            let mut one = Reader::from_str(xml);
            let (text, runs) = match one.read_event()? {
                Event::Start(_) => string_item(&mut one)?,
                Event::Empty(_) => Default::default(),
                _ => anyhow::bail!("无效共享字符串条目"),
            };
            if !runs.is_empty() {
                table.rich.insert(table.text.ends.len(), runs);
            }
            table.text.push(text);
        }
    }
    ensure!(opened && closed, "sharedStrings.xml 未正常结束");
    Ok(())
}

#[derive(Default)]
struct Chunk {
    first_row: u32,
    xml: String,
}

struct Rows {
    local: Loaded,
    cells: RowCells,
    shared: Formulas,
    shell: String,
}

impl Rows {
    fn decode(chunk: Chunk, table: &Strings, name: &str, part: &str) -> Result<Self> {
        let mut local = Loaded::empty(name, part);
        let mut cells = Vec::new();
        let mut shared = Formulas::default();
        let shell = parse_section(
            &chunk.xml,
            table,
            &mut local,
            calamine::SheetType::WorkSheet,
            &mut shared,
            Some(&mut cells),
            chunk.first_row,
        )?;
        Ok(Self {
            local,
            cells,
            shared,
            shell,
        })
    }

    fn append(self, out: &mut Loaded, formulas: &mut Formulas, shell: &mut String) -> Result<()> {
        for ((r, c), cell) in self.cells {
            out.sheet.set(r, c, cell);
        }
        out.xfs.extend(self.local.xfs);
        out.sheet.rich.extend(self.local.sheet.rich);
        out.sheet.arrays.extend(self.local.sheet.arrays);
        out.sheet.spill_shapes.extend(self.local.sheet.spill_shapes);
        for p in self.local.diag.problems {
            if !out.diag.problems.iter().any(|old| old.what == p.what) {
                out.diag.problems.push(p);
            }
        }
        for (index, master) in self.shared.masters {
            ensure!(
                formulas.masters.insert(index, master).is_none(),
                "共享公式序号重复"
            );
        }
        formulas.children.extend(self.shared.children);
        shell.push_str(&self.shell);
        Ok(())
    }
}

fn flush(
    chunks: &mut Vec<Chunk>,
    table: &Strings,
    out: &mut Loaded,
    formulas: &mut Formulas,
    shell: &mut String,
) -> Result<()> {
    if chunks.len() == 1 {
        let chunk = chunks.pop().unwrap();
        let rows = Rows::decode(
            chunk,
            table,
            &out.sheet.name,
            out.sheet.source_part.as_deref().unwrap_or(""),
        )?;
        return rows.append(out, formulas, shell);
    }
    if chunks.is_empty() {
        return Ok(());
    }
    let name = out.sheet.name.clone();
    let part = out.sheet.source_part.clone().unwrap_or_default();
    std::thread::scope(|pool| -> Result<()> {
        let jobs: Vec<_> = chunks
            .drain(..)
            .map(|chunk| {
                let (name, part) = (&name, &part);
                pool.spawn(move || Rows::decode(chunk, table, name, part))
            })
            .collect();
        for job in jobs {
            job.join()
                .map_err(|_| anyhow::anyhow!("行块读取线程异常退出"))??
                .append(out, formulas, shell)?;
        }
        Ok(())
    })
}

pub(super) fn worksheet(
    input: impl Read,
    table: &Strings,
    out: &mut Loaded,
    workers: usize,
    chunk_size: usize,
) -> Result<String> {
    let mut reader = Window::start(input, &mut io::sink())?;
    let (mut token, mut skip) = (Vec::new(), Vec::new());
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunk = Chunk::default();
    let mut shared = Formulas::default();
    let mut shell = String::new();
    let (mut opened, mut closed, mut in_data, mut saw_data) = (false, false, false, false);
    let mut next_row = 0u32;
    loop {
        token.clear();
        let event = reader.read_event_into(&mut token)?;
        let mut row = None;
        let mut between_rows = in_data;
        match event {
            Event::Start(ref t) | Event::Empty(ref t) => {
                let empty = matches!(event, Event::Empty(_));
                if !opened {
                    ensure!(
                        t.local_name().as_ref() == "worksheet",
                        "工作表缺少 worksheet 根元素"
                    );
                    opened = true;
                    closed = empty;
                } else {
                    ensure!(!closed, "工作表存在多个根元素");
                    match (in_data, t.local_name().as_ref()) {
                        (false, "sheetData") => {
                            ensure!(!saw_data, "重复的 sheetData");
                            saw_data = true;
                            in_data = !empty;
                        }
                        (true, "row") => {
                            let i = if let Some(r) = attr(t, "r") {
                                r.parse::<u32>()?.checked_sub(1).context("行号为零")?
                            } else {
                                next_row
                            };
                            next_row = i.saturating_add(1);
                            row = Some(i);
                            if !empty {
                                skip.clear();
                                reader.read_to_end_into(t.name(), &mut skip)?;
                            }
                        }
                        _ => {
                            between_rows = false;
                            if !empty {
                                skip.clear();
                                reader.read_to_end_into(t.name(), &mut skip)?;
                            }
                        }
                    }
                }
            }
            Event::End(_) => {
                between_rows = false;
                if in_data {
                    in_data = false;
                } else {
                    closed = true;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        let end = reader.buffer_position();
        let xml = std::str::from_utf8(reader.get_mut().through(end)?)?;
        if let Some(r) = row {
            if chunk.xml.is_empty() {
                chunk.first_row = r;
            }
            chunk.xml.push_str(xml);
        } else if between_rows && !chunk.xml.is_empty() {
            chunk.xml.push_str(xml);
        } else if between_rows && !chunks.is_empty() {
            chunks.last_mut().unwrap().xml.push_str(xml);
        } else {
            if !chunk.xml.is_empty() {
                chunks.push(std::mem::take(&mut chunk));
            }
            flush(&mut chunks, table, out, &mut shared, &mut shell)?;
            shell.push_str(xml);
        }
        if chunk.xml.len() >= chunk_size {
            chunks.push(std::mem::take(&mut chunk));
            if chunks.len() >= workers {
                flush(&mut chunks, table, out, &mut shared, &mut shell)?;
            }
        }
    }
    ensure!(
        opened && closed && saw_data && !in_data,
        "工作表 XML 不完整"
    );
    expand_shared(out, shared)?;
    Ok(shell)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct CutConnection;
    impl Read for CutConnection {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("connection interrupted"))
        }
    }

    #[test]
    fn shared_text_offsets_stay_valid_across_slabs_and_empty_items() {
        let mut table = Strings::default();
        let labels = [
            "a".repeat(62000),
            String::new(),
            "b".repeat(5000),
            "长".repeat(30000),
            String::new(),
            "尾".to_owned(),
        ];
        for label in &labels {
            table.text.push(label.clone());
        }
        for (i, label) in labels.iter().enumerate() {
            assert_eq!(table.text.get(i), Some(label.as_str()));
        }
        assert!(table.text.get(labels.len()).is_none());
    }

    #[test]
    fn decodes_complete_rows_and_string_items_before_the_end_of_the_entry() {
        let input = "<worksheet><sheetData><row><c><v>41</v></c></row>";
        let mut out = Loaded::empty("one", "sheet");
        let table = Strings::default();
        assert!(
            worksheet(
                input.as_bytes().chain(CutConnection),
                &table,
                &mut out,
                1,
                1
            )
            .is_err()
        );
        assert_eq!(out.sheet.value(0, 0), crate::Value::Number(41.));
        let input = "<sst><si><t>first</t></si><si/><si><t>third</t></si>";
        let mut table = Strings::default();
        assert!(string_table(input.as_bytes().chain(CutConnection), &mut table).is_err());
        assert_eq!(table.text.get(0), Some("first"));
        assert_eq!(table.text.get(1), Some(""));
        assert_eq!(table.text.get(2), Some("third"));
    }
}
