use sheetcore::csv::{self, Encoding};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(super) struct ParsedCsv {
    pub rows: Vec<Vec<String>>,
    pub cols: usize,
    pub delim: char,
    pub encoding: Encoding,
}

pub(super) fn read_csv(
    path: &Path,
    delim: Option<char>,
    encoding: Option<Encoding>,
    quote: char,
) -> Result<ParsedCsv, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读不了 {}：{e}", path.display()))?;
    let (text, encoding, bad) = csv::decode(&bytes, encoding);
    if bad {
        return Err("所选编码无法完整解码这个文件".into());
    }
    let delim = match delim {
        None => csv::sniff(&text),
        Some(',') if !text.lines().next().unwrap_or("").contains(',') => csv::sniff(&text),
        Some(ch) => ch,
    };
    let rows = csv::records_strict_q(&text, delim, quote)?;
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    Ok(ParsedCsv {
        rows,
        cols,
        delim,
        encoding,
    })
}

pub(super) struct CsvDlg {
    pub path: PathBuf,
    pub delim: char,
    pub header: bool,
    pub enc: Option<Encoding>,
    pub used_encoding: Encoding,
    pub quote: char,
    pub text_cols: std::collections::BTreeSet<u32>,
    pub preview: Vec<Vec<String>>,
    pub cols: usize,
    pub err: Option<String>,
    pending: Option<Receiver<Result<ParsedCsv, String>>>,
}

impl CsvDlg {
    pub fn new(path: &Path) -> Self {
        let mut dlg = Self {
            path: path.to_owned(),
            delim: ',',
            header: false,
            enc: None,
            used_encoding: Encoding::Utf8,
            quote: '"',
            text_cols: Default::default(),
            preview: Vec::new(),
            cols: 0,
            err: None,
            pending: None,
        };
        dlg.reload();
        dlg
    }

    pub fn reload(&mut self) {
        self.preview = Vec::new();
        self.cols = 0;
        self.err = None;
        let path = self.path.clone();
        let encoding = self.enc;
        let quote = self.quote;
        // Small files are cheaper to read immediately. Large files are decoded and
        // validated on a worker before the user can commit their preview.
        if path.metadata().is_ok_and(|m| m.len() > 128 * 1024) {
            let (tx, rx) = mpsc::channel();
            self.pending = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(read_csv(&path, None, encoding, quote));
            });
        } else {
            self.apply(read_csv(&path, None, encoding, quote));
        }
    }

    pub fn poll_loading(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(data) => data,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => Err("CSV 读取线程已退出".into()),
        };
        self.pending = None;
        self.apply(result);
        false
    }

    fn apply(&mut self, result: Result<ParsedCsv, String>) {
        match result {
            Ok(data) => {
                self.preview = data.rows;
                self.cols = data.cols;
                self.delim = data.delim;
                self.used_encoding = data.encoding;
            }
            Err(err) => self.err = Some(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_csv_validates_tail_and_preserves_columns_outside_preview() {
        let env = crate::test_env::TestEnv::new("csv_large_preview");
        let path = env.path("large.csv");
        let mut csv = "1,2\n".repeat(34_000);
        csv.push_str("late,wide,row\n");
        std::fs::write(&path, &csv).unwrap();
        let mut dialog = CsvDlg::new(&path);
        assert!(dialog.pending.is_some());
        // Wait for this dialog's worker without mutating the UI or workbook.
        let result = dialog.pending.take().unwrap().recv().unwrap();
        dialog.apply(result);
        assert_eq!(dialog.cols, 3);
        assert_eq!(dialog.preview.len(), 34_001);
        csv.push_str("\"unterminated");
        std::fs::write(&path, csv).unwrap();
        dialog.reload();
        let result = dialog.pending.take().unwrap().recv().unwrap();
        dialog.apply(result);
        assert!(dialog.err.is_some());
        assert!(dialog.preview.is_empty());
    }
}
