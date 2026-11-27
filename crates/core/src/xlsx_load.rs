//! XLSX workers share one decoded string table and lock-free style lookup.
//! Large worksheet entries stream via byte-sliced row chunks into persistent workers.
#[path = "xlsx_read_stream.rs"]
pub(crate) mod stream;
use super::LoadDiag;
use crate::model::{Cell, CellRef, Sheet, Value};
use crate::richtext::RichRun;
use anyhow::{Context, Result, ensure};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU16, Ordering},
    },
};

/// Cross-layer-1 bridge: `Value::Text(Arc::clone(s))` makes every `t="s"` SST cell O(1) zero-heap-allocation.
#[inline]
fn shared_text_value(s: &Arc<str>) -> Value {
    Value::Text(Arc::clone(s))
}

/// Fast-path parser for `<= 15`-sig-digit ASCII numbers/decimals (exact Clinger IEEE-754 `f64`),
/// falling back to `core::num::dec2flt` (`str::parse::<f64>()`) for >15 digits or exponents.
#[inline]
pub(super) fn fast_parse_f64(s: &str) -> Result<f64, std::num::ParseFloatError> {
    if let Some(v) = stream::fast_parse_f64_bytes(s.as_bytes()) {
        return Ok(v);
    }
    s.parse::<f64>()
}

fn signature<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, part: &str) -> Option<(u32, u64)> {
    let e = zip.by_index_raw(zip.index_for_name(part)?).ok()?;
    Some((e.crc32(), e.size()))
}

#[cfg(windows)]
mod mmap_win {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    type Handle = *mut core::ffi::c_void;
    const PAGE_READONLY: u32 = 0x02;
    const FILE_MAP_READ: u32 = 0x04;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileMappingW(
            h_file: Handle,
            attrs: *const core::ffi::c_void,
            protect: u32,
            max_high: u32,
            max_low: u32,
            name: *const u16,
        ) -> Handle;
        fn MapViewOfFile(
            mapping: Handle,
            access: u32,
            off_high: u32,
            off_low: u32,
            bytes: usize,
        ) -> *const u8;
        fn UnmapViewOfFile(base: *const core::ffi::c_void) -> i32;
        fn CloseHandle(obj: Handle) -> i32;
    }

    pub(in crate::xlsx) struct MappedFile {
        ptr: ptr::NonNull<u8>,
        len: usize,
        _file: File,
    }

    unsafe impl Send for MappedFile {}
    unsafe impl Sync for MappedFile {}

    impl MappedFile {
        pub(super) fn try_map(file: File, len: usize) -> Option<Self> {
            if len == 0 {
                return None;
            }
            unsafe {
                let mapping = CreateFileMappingW(
                    file.as_raw_handle() as Handle,
                    ptr::null(),
                    PAGE_READONLY,
                    0,
                    0,
                    ptr::null(),
                );
                if mapping.is_null() {
                    return None;
                }
                let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len);
                let _ = CloseHandle(mapping);
                let ptr = ptr::NonNull::new(view as *mut u8)?;
                Some(Self {
                    ptr,
                    len,
                    _file: file,
                })
            }
        }

        #[inline]
        pub(super) fn as_slice(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
        }
    }

    impl Drop for MappedFile {
        fn drop(&mut self) {
            unsafe {
                let _ = UnmapViewOfFile(self.ptr.as_ptr() as *const core::ffi::c_void);
            }
        }
    }
}

#[allow(dead_code)]
pub(super) enum ArchiveBytes<'a> {
    #[cfg(windows)]
    Mapped(mmap_win::MappedFile),
    Owned(Vec<u8>),
    Borrowed(&'a [u8]),
}

impl<'a> ArchiveBytes<'a> {
    pub(super) fn open(path: &Path) -> Result<ArchiveBytes<'static>> {
        let file = File::open(path)?;
        #[cfg(windows)]
        {
            if let Ok(meta) = file.metadata() {
                if let Ok(len) = usize::try_from(meta.len()) {
                    if len > 0 {
                        if let Some(mapped) = mmap_win::MappedFile::try_map(file, len) {
                            return Ok(ArchiveBytes::Mapped(mapped));
                        }
                    } else {
                        return Ok(ArchiveBytes::Owned(Vec::new()));
                    }
                }
            }
        }
        let _ = file;
        Ok(ArchiveBytes::Owned(std::fs::read(path)?))
    }

    #[inline]
    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(windows)]
            Self::Mapped(m) => m.as_slice(),
            Self::Owned(v) => v.as_slice(),
            Self::Borrowed(b) => b,
        }
    }
}

pub(super) fn open_bytes_stamped<'a>(
    bytes: &'a [u8],
    compute_stamp: bool,
) -> Result<(zip::ZipArchive<Cursor<&'a [u8]>>, Option<[u8; 32]>)> {
    use sha2::{Digest, Sha256};
    let zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let stamp = if compute_stamp {
        let start = usize::try_from(zip.central_directory_start()).unwrap_or(bytes.len());
        let slice = bytes.get(start..).unwrap_or(&[]);
        Some(Sha256::digest(slice).into())
    } else {
        None
    };
    Ok((zip, stamp))
}

/// The central directory includes each part name, size, CRC and offset. Comparing
/// it before and after the load covers relationship files read by side workers too.
pub(super) fn package_stamp(path: &Path) -> Result<[u8; 32]> {
    let zip = zip::ZipArchive::new(File::open(path)?)?;
    let offset = zip.central_directory_start();
    hash_directory(&mut zip.into_inner(), offset)
}

fn hash_directory(file: &mut File, offset: u64) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::SeekFrom;
    file.seek(SeekFrom::Start(offset))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}

/// Default Office 2007 theme palette used by `calamine` when `xl/theme/theme1.xml` is absent,
/// already ordered by Excel's `theme` attribute (0=lt1, 1=dk1, 2=lt2, 3=dk2, 4..11=accent/links).
const CALAMINE_DEFAULT_THEME: [[u8; 3]; 12] = [
    [255, 255, 255],
    [0, 0, 0],
    [238, 236, 225],
    [31, 73, 125],
    [79, 129, 189],
    [192, 80, 77],
    [155, 187, 89],
    [128, 100, 162],
    [75, 172, 198],
    [247, 150, 70],
    [0, 0, 255],
    [128, 0, 128],
];

fn builtin_format_code_by_id(id: u32) -> Option<&'static str> {
    match id {
        0 => Some("General"),
        1 => Some("0"),
        2 => Some("0.00"),
        3 => Some("#,##0"),
        4 => Some("#,##0.00"),
        9 => Some("0%"),
        10 => Some("0.00%"),
        11 => Some("0.00E+00"),
        12 => Some("# ?/?"),
        13 => Some("# ??/??"),
        14 => Some("mm-dd-yy"),
        15 => Some("d-mmm-yy"),
        16 => Some("d-mmm"),
        17 => Some("mmm-yy"),
        18 => Some("h:mm AM/PM"),
        19 => Some("h:mm:ss AM/PM"),
        20 => Some("h:mm"),
        21 => Some("h:mm:ss"),
        22 => Some("m/d/yy h:mm"),
        37 => Some("#,##0 ;(#,##0)"),
        38 => Some("#,##0 ;[Red](#,##0)"),
        39 => Some("#,##0.00;(#,##0.00)"),
        40 => Some("#,##0.00;[Red](#,##0.00)"),
        41 => Some("_(* #,##0_);_(* (#,##0);_(* \"-\"_);_(@_)"),
        42 => Some("_($* #,##0_);_($* (#,##0);_($* \"-\"_);_(@_)"),
        43 => Some("_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)"),
        44 => Some("_($* #,##0.00_);_($* (#,##0.00);_($* \"-\"??_);_(@_)"),
        45 => Some("mm:ss"),
        46 => Some("[h]:mm:ss"),
        47 => Some("mmss.0"),
        48 => Some("##0.0E+0"),
        49 => Some("@"),
        _ => None,
    }
}

#[inline]
fn local_attr_name(raw: &str) -> &str {
    raw.rsplit_once(':').map_or(raw, |(_, l)| l)
}

#[inline]
fn parse_ooxml_bool(val: &str) -> bool {
    !matches!(val, "0" | "false")
}

#[inline]
fn parse_hex_argb(s: &str) -> Option<(u8, [u8; 3])> {
    let t = s.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    let pair = |off: usize| -> Option<u8> { u8::from_str_radix(t.get(off..off + 2)?, 16).ok() };
    match t.len() {
        6 => Some((255, [pair(0)?, pair(2)?, pair(4)?])),
        8 => Some((pair(0)?, [pair(2)?, pair(4)?, pair(6)?])),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RawColor {
    rgb: Option<(u8, [u8; 3])>,
    theme: Option<u8>,
    indexed: Option<u8>,
    tint: f64,
    has_tint: bool,
}

impl RawColor {
    fn from_elem(e: &BytesStart<'_>) -> Self {
        let mut c = Self::default();
        for a in e.attributes().flatten() {
            match local_attr_name(a.key.as_ref()) {
                "rgb" => c.rgb = parse_hex_argb(&a.value),
                "theme" => {
                    if let Ok(n) = a.value.trim().parse::<u32>() {
                        c.theme = Some(n.min(255) as u8);
                    }
                }
                "indexed" => {
                    if let Ok(n) = a.value.trim().parse::<u32>() {
                        c.indexed = Some(n.min(255) as u8);
                    }
                }
                "tint" => {
                    if let Ok(f) = a.value.trim().parse::<f64>() {
                        c.tint = f;
                        c.has_tint = true;
                    }
                }
                _ => {}
            }
        }
        c
    }

    #[inline]
    fn tint_f32(&self) -> f32 {
        if self.has_tint { self.tint as f32 } else { 0.0 }
    }

    /// Resolve color using `calamine` priority (`rgb > theme > indexed`), preserving alpha.
    fn resolve_calamine(&self, calamine_theme: &[[u8; 3]]) -> Option<(u8, [u8; 3])> {
        let (alpha, base) = if let Some((a, rgb)) = self.rgb {
            (a, rgb)
        } else if let Some(t) = self.theme {
            (255, calamine_theme.get(t as usize).copied().unwrap_or([0, 0, 0]))
        } else if let Some(idx) = self.indexed {
            let rgb = match idx {
                0..=63 => crate::style_theme::DEFAULT_INDEXED_PALETTE[idx as usize],
                64 => [0, 0, 0],
                65 => [255, 255, 255],
                _ => [0, 0, 0],
            };
            (255, rgb)
        } else {
            return None;
        };
        let rgb = if self.has_tint && self.tint != 0.0 {
            crate::style_theme::tint_rgb(base, self.tint as f32)
        } else {
            base
        };
        Some((alpha, rgb))
    }

    /// Resolve color using `style_theme` priority (`theme > rgb > custom indexed < 64`).
    fn resolve_theme(
        &self,
        palette: &[[u8; 3]],
        indexed_palette: &[[u8; 3]; 64],
    ) -> (Option<crate::style_theme::ThemeRef>, Option<[u8; 3]>) {
        let tint = self.tint_f32();
        let theme = self
            .theme
            .map(|idx| crate::style_theme::ThemeRef { idx, tint });
        let rgb = if let Some(t) = theme {
            palette
                .get(crate::style_theme::scheme_index(t.idx))
                .map(|base| crate::style_theme::tint_rgb(*base, t.tint))
        } else if let Some((_, hex)) = self.rgb {
            Some(crate::style_theme::tint_rgb(hex, tint))
        } else if let Some(idx) = self.indexed {
            if (idx as usize) < indexed_palette.len() {
                Some(crate::style_theme::tint_rgb(
                    indexed_palette[idx as usize],
                    tint,
                ))
            } else {
                None
            }
        } else {
            None
        };
        (theme, rgb)
    }
}

#[derive(Clone, Debug)]
struct ParsedFont {
    name: String,
    size: f32,
    bold: bool,
    italic: bool,
    underline: bool,
    double_underline: bool,
    strikethrough: bool,
    color: RawColor,
    has_color: bool,
}

impl Default for ParsedFont {
    fn default() -> Self {
        Self {
            name: String::new(),
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            double_underline: false,
            strikethrough: false,
            color: RawColor::default(),
            has_color: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ParsedFill {
    pattern_type: String,
    fg: RawColor,
    has_fg: bool,
    bg: RawColor,
    has_bg: bool,
}

#[inline]
fn is_calamine_fill_pattern(val: &str) -> bool {
    matches!(
        val,
        "solid"
            | "darkGray"
            | "mediumGray"
            | "lightGray"
            | "gray125"
            | "gray0625"
            | "darkHorizontal"
            | "darkVertical"
            | "darkDown"
            | "darkUp"
            | "darkGrid"
            | "darkTrellis"
            | "lightHorizontal"
            | "lightVertical"
            | "lightDown"
            | "lightUp"
            | "lightGrid"
            | "lightTrellis"
    )
}

#[inline]
fn parse_border_style_code(val: &str) -> u8 {
    match val {
        "thin" => 1,
        "medium" => 2,
        "thick" => 3,
        "double" => 4,
        "dashed" => 5,
        "dotted" => 6,
        "hair" => 7,
        "dashDot" => 8,
        "dashDotDot" => 9,
        "mediumDashed" => 10,
        "mediumDashDot" => 11,
        "mediumDashDotDot" => 12,
        "slantDashDot" => 13,
        _ => 0,
    }
}

#[derive(Clone, Debug, Default)]
struct ParsedBorder {
    sides: [(u8, RawColor, bool); 4],
    diagonal: Option<crate::style_ext::DiagBorder>,
}

#[derive(Clone, Copy, Debug)]
struct ParsedRawXf {
    num_fmt_id: Option<u32>,
    font_id: Option<u32>,
    fill_id: Option<u32>,
    border_id: Option<u32>,
    quote_prefix: bool,
    has_alignment: bool,
    halign: u8,
    valign: u8,
    wrap: bool,
    shrink_to_fit: bool,
    indent: u8,
    vertical_text: bool,
    rotation: i16,
    has_protection: bool,
    locked: bool,
    hidden: bool,
}

impl Default for ParsedRawXf {
    fn default() -> Self {
        Self {
            num_fmt_id: None,
            font_id: None,
            fill_id: None,
            border_id: None,
            quote_prefix: false,
            has_alignment: false,
            halign: 0,
            valign: 0,
            wrap: false,
            shrink_to_fit: false,
            indent: 0,
            vertical_text: false,
            rotation: 0,
            has_protection: false,
            locked: true,
            hidden: false,
        }
    }
}

/// Single-pass parsed representation of `xl/styles.xml` + `xl/theme/*.xml`.
/// Replaces the 6-pass `calamine` fake-ZIP + `style_theme` + `style_ext` + `cf::dxfs` pipeline.
#[derive(Clone, Debug)]
pub(super) struct ParsedStylesBook {
    theme_palette: Vec<[u8; 3]>,
    calamine_theme: Vec<[u8; 3]>,
    indexed_palette: [[u8; 3]; 64],
    num_fmts: HashMap<u32, String>,
    fonts: Vec<ParsedFont>,
    fills: Vec<ParsedFill>,
    borders: Vec<ParsedBorder>,
    xfs: Vec<ParsedRawXf>,
    pub dxfs: Vec<crate::cf::Dxf>,
    pub dxf_ext: bool,
}

impl Default for ParsedStylesBook {
    fn default() -> Self {
        Self {
            theme_palette: crate::style_theme::DEFAULT_THEME_PALETTE.to_vec(),
            calamine_theme: CALAMINE_DEFAULT_THEME.to_vec(),
            indexed_palette: crate::style_theme::DEFAULT_INDEXED_PALETTE,
            num_fmts: HashMap::new(),
            fonts: Vec::new(),
            fills: Vec::new(),
            borders: Vec::new(),
            xfs: Vec::new(),
            dxfs: Vec::new(),
            dxf_ext: false,
        }
    }
}

impl ParsedStylesBook {
    #[inline]
    pub(super) fn xf_count(&self) -> usize {
        self.xfs.len()
    }

    pub(super) fn resolve_xf(
        &self,
        index: usize,
    ) -> (
        crate::style::CellStyle,
        crate::style_theme::XfTheme,
        Option<crate::style_ext::FillPattern>,
        bool,
    ) {
        let mut cs = crate::style::CellStyle {
            size: 11.0,
            color: [0, 0, 0],
            ..Default::default()
        };
        let Some(xf) = self.xfs.get(index) else {
            return (cs, crate::style_theme::XfTheme::default(), None, false);
        };

        let mut font_theme = None;
        if let Some(f) = xf.font_id.and_then(|id| self.fonts.get(id as usize)) {
            cs.bold = f.bold;
            cs.italic = f.italic;
            cs.underline = f.underline;
            cs.double_underline = f.double_underline;
            cs.strikethrough = f.strikethrough;
            if !f.name.is_empty() {
                cs.font.clone_from(&f.name);
            }
            cs.size = f.size;
            if f.has_color {
                if let Some((_, rgb)) = f.color.resolve_calamine(&self.calamine_theme) {
                    cs.color = rgb;
                }
                let (t, rgb) = f
                    .color
                    .resolve_theme(&self.theme_palette, &self.indexed_palette);
                font_theme = t;
                if let Some(t) = t {
                    if let Some(base) = self
                        .theme_palette
                        .get(crate::style_theme::scheme_index(t.idx))
                    {
                        cs.color = crate::style_theme::tint_rgb(*base, t.tint);
                    }
                } else if let Some(c) = rgb {
                    cs.color = c;
                }
            }
        }

        let mut fill_theme = None;
        let mut pattern = None;
        if let Some(fl) = xf.fill_id.and_then(|id| self.fills.get(id as usize)) {
            if !fl.pattern_type.is_empty() && fl.pattern_type != "none" {
                pattern = Some(crate::style_ext::FillPattern {
                    kind: fl.pattern_type.clone(),
                    fg: fl.has_fg.then(|| fl.fg.rgb.map(|(_, c)| c)).flatten(),
                    bg: fl.has_bg.then(|| fl.bg.rgb.map(|(_, c)| c)).flatten(),
                    fg_theme: fl
                        .has_fg
                        .then(|| fl.fg.theme.map(|i| (i, fl.fg.tint_f32())))
                        .flatten(),
                    bg_theme: fl
                        .has_bg
                        .then(|| fl.bg.theme.map(|i| (i, fl.bg.tint_f32())))
                        .flatten(),
                    fg_indexed: fl.has_fg.then(|| fl.fg.indexed).flatten(),
                    bg_indexed: fl.has_bg.then(|| fl.bg.indexed).flatten(),
                });
                if is_calamine_fill_pattern(&fl.pattern_type) && fl.has_fg {
                    if let Some((a, rgb)) = fl.fg.resolve_calamine(&self.calamine_theme) {
                        if !(rgb == [0, 0, 0] && a == 0) {
                            cs.fill = Some(rgb);
                        }
                    }
                }
                let (fg_t, fg_rgb) = if fl.has_fg {
                    fl.fg
                        .resolve_theme(&self.theme_palette, &self.indexed_palette)
                } else {
                    (None, None)
                };
                let (chosen_t, chosen_rgb) = if fg_t.is_some() || fg_rgb.is_some() {
                    (fg_t, fg_rgb)
                } else if fl.has_bg {
                    fl.bg
                        .resolve_theme(&self.theme_palette, &self.indexed_palette)
                } else {
                    (None, None)
                };
                fill_theme = chosen_t;
                if let Some(t) = chosen_t {
                    if let Some(base) = self
                        .theme_palette
                        .get(crate::style_theme::scheme_index(t.idx))
                    {
                        cs.fill = Some(crate::style_theme::tint_rgb(*base, t.tint));
                    }
                } else if let Some(c) = chosen_rgb {
                    cs.fill = Some(c);
                }
            }
        }

        if let Some(b) = xf.border_id.and_then(|id| self.borders.get(id as usize)) {
            for i in 0..4 {
                let (style_code, raw_col, has_col) = b.sides[i];
                let color = if has_col {
                    raw_col
                        .resolve_calamine(&self.calamine_theme)
                        .map_or([0, 0, 0], |(_, c)| c)
                } else {
                    [0, 0, 0]
                };
                cs.border[i] = crate::style::Border {
                    style: style_code,
                    color,
                };
            }
            cs.diagonal.clone_from(&b.diagonal);
        }

        if xf.has_protection {
            cs.locked = xf.locked;
            cs.hidden = xf.hidden;
        }

        if xf.has_alignment {
            cs.halign = xf.halign;
            cs.valign = xf.valign;
            cs.wrap = xf.wrap;
            cs.shrink_to_fit = xf.shrink_to_fit;
            cs.indent = xf.indent;
            cs.vertical_text = xf.vertical_text;
            cs.rotation = xf.rotation;
        }

        if let Some(id) = xf.num_fmt_id {
            let code = self
                .num_fmts
                .get(&id)
                .map(String::as_str)
                .or_else(|| builtin_format_code_by_id(id));
            if let Some(code) = code {
                if !code.is_empty() && code != "General" {
                    cs.format = code.to_owned();
                }
            }
        }

        (
            cs,
            crate::style_theme::XfTheme {
                font: font_theme,
                fill: fill_theme,
            },
            pattern,
            xf.quote_prefix,
        )
    }

    pub(super) fn parse_xml(styles_xml: &str, theme_xml: Option<&str>) -> Result<Self> {
        let mut book = Self::default();
        if let Some(txml) = theme_xml {
            let pal = crate::style_theme::parse_palette(txml);
            if !pal.is_empty() {
                book.calamine_theme = (0..pal.len())
                    .map(|i| {
                        pal.get(crate::style_theme::scheme_index(i as u8))
                            .copied()
                            .unwrap_or([0, 0, 0])
                    })
                    .collect();
                book.theme_palette = pal;
            }
        }
        if styles_xml.is_empty() {
            return Ok(book);
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Section {
            None,
            NumFmts,
            Fonts,
            Fills,
            Borders,
            CellXfs,
            Dxfs,
            Colors,
            Other,
        }

        let mut reader = Reader::from_str(styles_xml);
        let mut depth: usize = 0;
        let mut section = Section::None;

        let mut cur_font = ParsedFont::default();
        let mut in_font = false;

        let mut cur_fill = ParsedFill::default();
        let mut in_fill = false;

        let mut cur_border = ParsedBorder::default();
        let mut in_border = false;
        let mut cur_diag_up = false;
        let mut cur_diag_down = false;
        let mut cur_side: Option<usize> = None; // 0=L,1=R,2=T,3=B,4=diagonal
        let mut cur_diag_style = String::new();
        let mut cur_diag_color = RawColor::default();

        let mut cur_xf = ParsedRawXf::default();
        let mut in_xf = false;

        let mut in_indexed_colors = false;
        let mut indexed_slot: usize = 0;
        let mut saw_dxf = false;
        let mut dxfs_start: usize = 0;
        let mut dxfs_end: usize = 0;

        let parse_count = |e: &BytesStart<'_>| -> usize {
            for a in e.attributes().flatten() {
                if local_attr_name(a.key.as_ref()) == "count" {
                    if let Ok(n) = a.value.trim().parse::<usize>() {
                        return n.min(262_144);
                    }
                }
            }
            0
        };

        loop {
            let ev_start = reader.buffer_position() as usize;
            let ev = reader.read_event().context("xl/styles.xml 解析失败")?;
            match &ev {
                Event::Start(e) | Event::Empty(e) => {
                    let is_empty = matches!(ev, Event::Empty(_));
                    let raw_name = e.name();
                    if !book.dxf_ext {
                        let r = raw_name.as_ref();
                        if r.eq_ignore_ascii_case("diagonal")
                            || r.eq_ignore_ascii_case("vertical")
                            || r.eq_ignore_ascii_case("horizontal")
                            || r.eq_ignore_ascii_case("start")
                            || r.eq_ignore_ascii_case("end")
                            || r.eq_ignore_ascii_case("outline")
                        {
                            if e.attributes().flatten().any(|a| {
                                a.key.as_ref() == "style"
                                    && !a.value.is_empty()
                                    && a.value.as_ref() != "none"
                            }) {
                                book.dxf_ext = true;
                            }
                        }
                    }
                    let tag = e.local_name();
                    let lname = tag.as_ref();
                    let level = depth;
                    if !is_empty {
                        depth += 1;
                    }

                    if level == 0 {
                        continue;
                    }
                    if level == 1 {
                        if !is_empty {
                            section = match lname {
                                "numFmts" => {
                                    book.num_fmts.reserve(parse_count(e));
                                    Section::NumFmts
                                }
                                "fonts" => {
                                    book.fonts.reserve(parse_count(e));
                                    Section::Fonts
                                }
                                "fills" => {
                                    book.fills.reserve(parse_count(e));
                                    Section::Fills
                                }
                                "borders" => {
                                    book.borders.reserve(parse_count(e));
                                    Section::Borders
                                }
                                "cellXfs" => {
                                    book.xfs.reserve(parse_count(e));
                                    Section::CellXfs
                                }
                                "dxfs" => {
                                    dxfs_start = ev_start;
                                    Section::Dxfs
                                }
                                "colors" => Section::Colors,
                                _ => Section::Other,
                            };
                        }
                        continue;
                    }

                    match section {
                        Section::NumFmts if level == 2 && lname == "numFmt" => {
                            let mut id: Option<u32> = None;
                            let mut code: Option<String> = None;
                            for a in e.attributes().flatten() {
                                match local_attr_name(a.key.as_ref()) {
                                    "numFmtId" => id = a.value.trim().parse::<u32>().ok(),
                                    "formatCode" => code = Some(crate::model::xml_unescape(&a.value)),
                                    _ => {}
                                }
                            }
                            if let (Some(id), Some(code)) = (id, code) {
                                book.num_fmts.insert(id, code);
                            }
                        }
                        Section::Fonts => {
                            if level == 2 && lname == "font" {
                                cur_font = ParsedFont::default();
                                if is_empty {
                                    book.fonts.push(std::mem::take(&mut cur_font));
                                } else {
                                    in_font = true;
                                }
                            } else if in_font && level == 3 {
                                match lname {
                                    "name" => {
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                let decoded = crate::model::xml_unescape(&a.value);
                                                cur_font.name = crate::style::normalize_font(&decoded);
                                            }
                                        }
                                    }
                                    "sz" => {
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                if let Ok(sz) = a.value.trim().parse::<f64>() {
                                                    cur_font.size = sz as f32;
                                                }
                                            }
                                        }
                                    }
                                    "b" => {
                                        let mut val = None;
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                val = Some(a.value);
                                            }
                                        }
                                        cur_font.bold = match val.as_deref() {
                                            None => true,
                                            Some("bold" | "700" | "1" | "true") => true,
                                            Some("normal" | "400" | "0" | "false") => false,
                                            Some(v) => v.parse::<u16>().is_ok_and(|w| w >= 600),
                                        };
                                    }
                                    "i" => {
                                        let mut val = None;
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                val = Some(a.value);
                                            }
                                        }
                                        cur_font.italic = match val.as_deref() {
                                            None => true,
                                            Some("italic" | "oblique" | "1" | "true") => true,
                                            _ => false,
                                        };
                                    }
                                    "u" => {
                                        let mut val = None;
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                val = Some(a.value);
                                            }
                                        }
                                        match val.as_deref() {
                                            None | Some("single" | "singleAccounting" | "doubleAccounting") => {
                                                cur_font.underline = true;
                                                cur_font.double_underline = false;
                                            }
                                            Some("double") => {
                                                cur_font.underline = true;
                                                cur_font.double_underline = true;
                                            }
                                            _ => {
                                                cur_font.underline = false;
                                                cur_font.double_underline = false;
                                            }
                                        }
                                    }
                                    "strike" => {
                                        let mut val = None;
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "val" {
                                                val = Some(a.value);
                                            }
                                        }
                                        cur_font.strikethrough =
                                            val.as_deref().map_or(true, parse_ooxml_bool);
                                    }
                                    "color" => {
                                        cur_font.color = RawColor::from_elem(e);
                                        cur_font.has_color = true;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Section::Fills => {
                            if level == 2 && lname == "fill" {
                                cur_fill = ParsedFill::default();
                                if is_empty {
                                    book.fills.push(std::mem::take(&mut cur_fill));
                                } else {
                                    in_fill = true;
                                }
                            } else if in_fill {
                                match lname {
                                    "patternFill" => {
                                        for a in e.attributes().flatten() {
                                            if local_attr_name(a.key.as_ref()) == "patternType" {
                                                cur_fill.pattern_type = a.value.into_owned();
                                            }
                                        }
                                    }
                                    "fgColor" => {
                                        cur_fill.fg = RawColor::from_elem(e);
                                        cur_fill.has_fg = true;
                                    }
                                    "bgColor" => {
                                        cur_fill.bg = RawColor::from_elem(e);
                                        cur_fill.has_bg = true;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Section::Borders => {
                            if level == 2 && lname == "border" {
                                cur_border = ParsedBorder::default();
                                cur_diag_up = false;
                                cur_diag_down = false;
                                cur_side = None;
                                for a in e.attributes().flatten() {
                                    match local_attr_name(a.key.as_ref()) {
                                        "diagonalUp" => cur_diag_up = a.value == "1" || a.value == "true",
                                        "diagonalDown" => cur_diag_down = a.value == "1" || a.value == "true",
                                        _ => {}
                                    }
                                }
                                if is_empty {
                                    book.borders.push(std::mem::take(&mut cur_border));
                                } else {
                                    in_border = true;
                                }
                            } else if in_border && level == 3 {
                                let side_idx = match lname {
                                    "left" => Some(0),
                                    "right" => Some(1),
                                    "top" => Some(2),
                                    "bottom" => Some(3),
                                    "diagonal" => Some(4),
                                    _ => None,
                                };
                                if let Some(s) = side_idx {
                                    let mut style_attr = None;
                                    for a in e.attributes().flatten() {
                                        if local_attr_name(a.key.as_ref()) == "style" {
                                            style_attr = Some(a.value);
                                        }
                                    }
                                    if s < 4 {
                                        let code = style_attr
                                            .as_deref()
                                            .map_or(0, parse_border_style_code);
                                        cur_border.sides[s] = (code, RawColor::default(), false);
                                    } else {
                                        cur_diag_style = style_attr
                                            .map(|v| v.into_owned())
                                            .unwrap_or_default();
                                        cur_diag_color = RawColor::default();
                                        if is_empty
                                            && !cur_diag_style.is_empty()
                                            && cur_diag_style != "none"
                                        {
                                            cur_border.diagonal = Some(crate::style_ext::DiagBorder {
                                                style: std::mem::take(&mut cur_diag_style),
                                                color: [0, 0, 0],
                                                color_theme: None,
                                                up: cur_diag_up,
                                                down: cur_diag_down,
                                            });
                                        }
                                    }
                                    if !is_empty {
                                        cur_side = Some(s);
                                    }
                                }
                            } else if in_border && level == 4 && lname == "color" {
                                if let Some(s) = cur_side {
                                    let col = RawColor::from_elem(e);
                                    if s < 4 {
                                        cur_border.sides[s].1 = col;
                                        cur_border.sides[s].2 = true;
                                    } else {
                                        cur_diag_color = col;
                                    }
                                }
                            }
                        }
                        Section::CellXfs => {
                            if level == 2 && lname == "xf" {
                                cur_xf = ParsedRawXf::default();
                                for a in e.attributes().flatten() {
                                    match local_attr_name(a.key.as_ref()) {
                                        "numFmtId" => {
                                            cur_xf.num_fmt_id = a.value.trim().parse::<u32>().ok();
                                        }
                                        "fontId" => {
                                            cur_xf.font_id = a.value.trim().parse::<u32>().ok();
                                        }
                                        "fillId" => {
                                            cur_xf.fill_id = a.value.trim().parse::<u32>().ok();
                                        }
                                        "borderId" => {
                                            cur_xf.border_id = a.value.trim().parse::<u32>().ok();
                                        }
                                        "quotePrefix" => {
                                            cur_xf.quote_prefix =
                                                a.value == "1" || a.value == "true";
                                        }
                                        _ => {}
                                    }
                                }
                                if is_empty {
                                    book.xfs.push(cur_xf);
                                } else {
                                    in_xf = true;
                                }
                            } else if in_xf && level == 3 {
                                match lname {
                                    "alignment" => {
                                        cur_xf.has_alignment = true;
                                        // Matches calamine::Alignment::default() -> VerticalAlignment::Bottom.
                                        cur_xf.halign = 0;
                                        cur_xf.valign = 3;
                                        cur_xf.wrap = false;
                                        cur_xf.shrink_to_fit = false;
                                        cur_xf.indent = 0;
                                        cur_xf.vertical_text = false;
                                        cur_xf.rotation = 0;
                                        for a in e.attributes().flatten() {
                                            match local_attr_name(a.key.as_ref()) {
                                                "horizontal" => {
                                                    cur_xf.halign = match a.value.as_ref() {
                                                        "left" => 1,
                                                        "center" => 2,
                                                        "right" => 3,
                                                        "centerContinuous" => 4,
                                                        "distributed" => 5,
                                                        _ => 0,
                                                    };
                                                }
                                                "vertical" => {
                                                    cur_xf.valign = match a.value.as_ref() {
                                                        "top" => 1,
                                                        "center" => 2,
                                                        "bottom" => 3,
                                                        "justify" | "distributed" => 0,
                                                        _ => 3,
                                                    };
                                                }
                                                "wrapText" => {
                                                    cur_xf.wrap = parse_ooxml_bool(&a.value);
                                                }
                                                "shrinkToFit" => {
                                                    cur_xf.shrink_to_fit =
                                                        parse_ooxml_bool(&a.value);
                                                }
                                                "indent" => {
                                                    if let Ok(ind) = a.value.trim().parse::<u8>() {
                                                        cur_xf.indent = ind.min(15);
                                                    }
                                                }
                                                "textRotation" => {
                                                    if let Ok(d) = a.value.trim().parse::<u16>() {
                                                        if d == 255 {
                                                            cur_xf.vertical_text = true;
                                                            cur_xf.rotation = 0;
                                                        } else {
                                                            cur_xf.vertical_text = false;
                                                            cur_xf.rotation = if d <= 90 {
                                                                d as i16
                                                            } else {
                                                                90 - d as i16
                                                            };
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    "protection" => {
                                        cur_xf.has_protection = true;
                                        cur_xf.locked = true;
                                        cur_xf.hidden = false;
                                        for a in e.attributes().flatten() {
                                            match local_attr_name(a.key.as_ref()) {
                                                "locked" => {
                                                    cur_xf.locked = parse_ooxml_bool(&a.value);
                                                }
                                                "hidden" => {
                                                    cur_xf.hidden = parse_ooxml_bool(&a.value);
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Section::Dxfs => {
                            if level == 2 && lname == "dxf" {
                                saw_dxf = true;
                            }
                        }
                        Section::Colors => {
                            if level == 2 && lname == "indexedColors" {
                                if !is_empty {
                                    in_indexed_colors = true;
                                    indexed_slot = 0;
                                }
                            } else if in_indexed_colors && level == 3 && lname == "rgbColor" {
                                for a in e.attributes().flatten() {
                                    if local_attr_name(a.key.as_ref()) == "rgb" {
                                        if let Some((_, c)) = parse_hex_argb(&a.value) {
                                            if indexed_slot < book.indexed_palette.len() {
                                                book.indexed_palette[indexed_slot] = c;
                                            }
                                        }
                                    }
                                }
                                indexed_slot += 1;
                            }
                        }
                        Section::NumFmts | Section::None | Section::Other => {}
                    }
                }
                Event::End(e) => {
                    depth = depth.checked_sub(1).context("xl/styles.xml 标签不匹配")?;
                    let tag = e.local_name();
                    let lname = tag.as_ref();
                    if depth == 1 {
                        if section == Section::Dxfs {
                            dxfs_end = reader.buffer_position() as usize;
                        }
                        section = Section::None;
                    } else if depth == 2 {
                        match section {
                            Section::Fonts if in_font && lname == "font" => {
                                book.fonts.push(std::mem::take(&mut cur_font));
                                in_font = false;
                            }
                            Section::Fills if in_fill && lname == "fill" => {
                                book.fills.push(std::mem::take(&mut cur_fill));
                                in_fill = false;
                            }
                            Section::Borders if in_border && lname == "border" => {
                                book.borders.push(std::mem::take(&mut cur_border));
                                in_border = false;
                                cur_side = None;
                            }
                            Section::CellXfs if in_xf && lname == "xf" => {
                                book.xfs.push(cur_xf);
                                in_xf = false;
                            }
                            Section::Colors if in_indexed_colors && lname == "indexedColors" => {
                                in_indexed_colors = false;
                            }
                            _ => {}
                        }
                    } else if depth == 3 && section == Section::Borders && in_border {
                        if cur_side == Some(4) && lname == "diagonal" {
                            if !cur_diag_style.is_empty() && cur_diag_style != "none" {
                                cur_border.diagonal = Some(crate::style_ext::DiagBorder {
                                    style: std::mem::take(&mut cur_diag_style),
                                    color: cur_diag_color.rgb.map_or([0, 0, 0], |(_, c)| c),
                                    color_theme: cur_diag_color
                                        .theme
                                        .map(|i| (i, cur_diag_color.tint_f32())),
                                    up: cur_diag_up,
                                    down: cur_diag_down,
                                });
                            }
                        }
                        cur_side = None;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        ensure!(depth == 0, "xl/styles.xml 未正常闭合");
        if saw_dxf && dxfs_end > dxfs_start {
            book.dxfs = crate::cf::parse_dxfs(&styles_xml[dxfs_start..dxfs_end]);
        }
        Ok(book)
    }
}

pub(super) fn parse_styles_from_zip<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<ParsedStylesBook> {
    let theme_xml = if let Some(idx) = zip.index_for_name("xl/theme/theme1.xml") {
        zip.by_index(idx).ok().and_then(|mut f| {
            crate::fixups::read_zip_entry_string_limited(&mut f, crate::fixups::MAX_META_PART_BYTES).ok()
        })
    } else {
        let alt_name = zip
            .file_names()
            .find(|n| n.starts_with("xl/theme/") && n.ends_with(".xml"))
            .map(str::to_owned);
        alt_name.and_then(|name| {
            zip.by_name(&name).ok().and_then(|mut f| {
                crate::fixups::read_zip_entry_string_limited(&mut f, crate::fixups::MAX_META_PART_BYTES).ok()
            })
        })
    };
    let styles_xml = if zip.index_for_name("xl/styles.xml").is_some() {
        xml_part(zip, "xl/styles.xml")?
    } else {
        String::new()
    };
    ParsedStylesBook::parse_xml(&styles_xml, theme_xml.as_deref())
}

pub(super) struct Header {
    date_system: crate::DateSystem,
    dynamic: std::collections::HashSet<u32>,
    pub names: Vec<String>,
    types: Vec<calamine::SheetType>,
    pub parsed_styles: ParsedStylesBook,
}

#[derive(Default)]
pub(super) struct WorkbookMetadata {
    pub xml: String,
    pub targets: HashMap<String, String>,
    pub header: Option<Header>,
}

pub(super) fn workbook_metadata<R: Read + Seek>(
    src: &mut zip::ZipArchive<R>,
) -> Result<WorkbookMetadata> {
    let read = |zip: &mut zip::ZipArchive<R>, name: &str| -> Result<Option<Vec<u8>>> {
        let Some(index) = zip.index_for_name(name) else { return Ok(None) };
        let mut entry = zip.by_index(index)?;
        let bytes = crate::fixups::read_zip_entry_limited(&mut entry, crate::fixups::MAX_META_PART_BYTES)?;
        Ok(Some(bytes))
    };
    let Some(bytes) = read(src, "xl/workbook.xml")? else {
        return Ok(WorkbookMetadata::default());
    };
    let xml = decode_xml_part(bytes, "xl/workbook.xml").unwrap_or_default();
    let rels = read(src, "xl/_rels/workbook.xml.rels")?;
    let rels_text = rels
        .as_deref()
        .map(|b| decode_xml_part(b.to_vec(), "xl/_rels/workbook.xml.rels").unwrap_or_default());
    let targets = crate::patch::sheet_targets_xml(Some(&xml), rels_text.as_deref());
    let header = header(src, &targets, &xml, rels_text.as_deref(), rels.as_deref())?;
    Ok(WorkbookMetadata { xml, targets, header })
}

fn dynamic_indices_generic<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    rels: &[u8],
) -> Result<std::collections::HashSet<u32>> {
    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const XDA: &str = "http://schemas.microsoft.com/office/spreadsheetml/2017/dynamicarray";
    const REL: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata";
    const RELS: &str = "xl/_rels/workbook.xml.rels";

    let mut result = std::collections::HashSet::new();
    let relationships = roxmltree::Document::parse(std::str::from_utf8(rels)?)?;
    let Some(target) = relationships
        .root_element()
        .children()
        .find(|r| r.attribute("Type") == Some(REL) && r.attribute("TargetMode") != Some("External"))
        .and_then(|r| r.attribute("Target"))
    else {
        return Ok(result);
    };
    let body = super::read_zip_text(zip, &crate::fixups::resolve_target(RELS, target))?;
    let doc = roxmltree::Document::parse(&body)?;
    let root = doc.root_element();
    let child = |n| root.children().find(|e| e.has_tag_name((NS, n)));
    let (Some(types), Some(cells)) = (child("metadataTypes"), child("cellMetadata")) else {
        return Ok(result);
    };
    let Some(t) = types
        .children()
        .filter(|n| n.has_tag_name((NS, "metadataType")))
        .position(|n| n.attribute("name") == Some("XLDAPR"))
    else {
        return Ok(result);
    };
    let dynamic: std::collections::HashSet<_> = root
        .children()
        .filter(|n| n.has_tag_name((NS, "futureMetadata")) && n.attribute("name") == Some("XLDAPR"))
        .flat_map(|n| n.children().filter(|n| n.has_tag_name((NS, "bk"))))
        .enumerate()
        .filter(|(_, n)| {
            n.descendants().any(|p| {
                p.has_tag_name((XDA, "dynamicArrayProperties"))
                    && matches!(p.attribute("fDynamic"), Some("1" | "true"))
            })
        })
        .map(|(i, _)| i)
        .collect();
    for (i, block) in cells
        .children()
        .filter(|n| n.has_tag_name((NS, "bk")))
        .enumerate()
    {
        if block.children().any(|r| {
            r.has_tag_name((NS, "rc"))
                && r.attribute("t").and_then(|a| a.parse().ok()) == Some(t + 1)
                && r.attribute("v")
                    .and_then(|a| a.parse::<usize>().ok())
                    .is_some_and(|v| dynamic.contains(&v))
        }) {
            result.insert(u32::try_from(i + 1)?);
        }
    }
    Ok(result)
}

/// Direct single-pass OOXML header & style loader: parses `xl/workbook.xml`,
/// `xl/_rels/workbook.xml.rels`, `xl/theme/*.xml` and `xl/styles.xml` directly from
/// the opened ZIP archive without constructing a synthetic in-memory ZIP for `calamine`.
pub(super) fn header<R: Read + Seek>(
    src: &mut zip::ZipArchive<R>,
    targets: &HashMap<String, String>,
    workbook: &str,
    rels_text: Option<&str>,
    rels_raw: Option<&[u8]>,
) -> Result<Option<Header>> {
    if targets.is_empty() || !targets.values().any(|p| src.index_for_name(p).is_some()) {
        return Ok(None);
    }
    let Some(rels_str) = rels_text else {
        return Ok(None);
    };

    // Parse workbook.xml.rels -> relationship Id -> Type
    let mut rel_types: HashMap<String, String> = HashMap::new();
    {
        let mut r = Reader::from_str(rels_str);
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e))
                    if e.local_name().as_ref() == "Relationship" =>
                {
                    let mut id = None;
                    let mut typ = None;
                    let mut external = false;
                    for a in e.attributes().flatten() {
                        match local_attr_name(a.key.as_ref()) {
                            "Id" => id = Some(a.value.into_owned()),
                            "Type" => typ = Some(a.value.into_owned()),
                            "TargetMode" => external = a.value.eq_ignore_ascii_case("External"),
                            _ => {}
                        }
                    }
                    if !external {
                        if let (Some(id), Some(typ)) = (id, typ) {
                            rel_types.insert(id, typ);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => return Ok(None),
                _ => {}
            }
        }
    }

    // Parse workbook.xml -> sheet names + SheetType in document order
    let mut names = Vec::new();
    let mut types = Vec::new();
    {
        let mut r = Reader::from_str(workbook);
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e))
                    if e.local_name().as_ref() == "sheet" =>
                {
                    let mut name: Option<String> = None;
                    let mut rel_id: Option<String> = None;
                    let mut valid_state = true;
                    for a in e.attributes().flatten() {
                        match local_attr_name(a.key.as_ref()) {
                            "name" => name = Some(crate::model::xml_unescape(&a.value)),
                            "id" => rel_id = Some(a.value.into_owned()),
                            "state" => {
                                valid_state =
                                    matches!(a.value.as_ref(), "visible" | "hidden" | "veryHidden");
                            }
                            _ => {}
                        }
                    }
                    if !valid_state {
                        return Ok(None);
                    }
                    let (Some(name), Some(rid)) = (name, rel_id) else {
                        return Ok(None);
                    };
                    let Some(rel_type) = rel_types.get(&rid) else {
                        return Ok(None);
                    };
                    let typ = match rel_type.rsplit('/').next() {
                        Some("worksheet") => calamine::SheetType::WorkSheet,
                        Some("chartsheet") => calamine::SheetType::ChartSheet,
                        Some("dialogsheet") => calamine::SheetType::DialogSheet,
                        _ => return Ok(None),
                    };
                    names.push(name);
                    types.push(typ);
                }
                Ok(Event::Eof) => break,
                Err(_) => return Ok(None),
                _ => {}
            }
        }
    }

    if names.is_empty() || names.iter().any(|n| !targets.contains_key(n)) {
        return Ok(None);
    }

    let parsed_styles = parse_styles_from_zip(src)?;
    let dynamic = if rels_str.contains("sheetMetadata") {
        if let Some(raw) = rels_raw {
            dynamic_indices_generic(src, raw)?
        } else {
            Default::default()
        }
    } else {
        Default::default()
    };
    Ok(Some(Header {
        date_system: if crate::patch::date1904_xml(workbook) { crate::DateSystem::D1904 } else { crate::DateSystem::D1900 },
        dynamic,
        names,
        types,
        parsed_styles,
    }))
}

/// Concurrent lock-free XF -> StyleTable ID resolver.
/// Only interns XFs actually referenced by worksheet cells/rows/cols, preserving
/// exact `StyleTable` and `StyleMap` uniqueness semantics while eliminating `Loaded::xfs`.
pub(super) struct StyleLookup<'a> {
    cache: Vec<AtomicU16>,
    inner: Mutex<(crate::style::StyleTable, HashMap<u16, u16>)>,
    parsed: &'a ParsedStylesBook,
}

impl<'a> StyleLookup<'a> {
    pub(super) fn new(
        styles: crate::style::StyleTable,
        ids: HashMap<u16, u16>,
        parsed: &'a ParsedStylesBook,
    ) -> Self {
        let cap = parsed.xf_count().saturating_add(16).max(64);
        let cache: Vec<AtomicU16> = (0..cap).map(|_| AtomicU16::new(u16::MAX)).collect();
        for (&xf, &id) in &ids {
            if let Some(slot) = cache.get(xf as usize) {
                slot.store(id, Ordering::Relaxed);
            }
        }
        Self {
            cache,
            inner: Mutex::new((styles, ids)),
            parsed,
        }
    }

    #[inline]
    pub(super) fn resolve(&self, xf: u16) -> u16 {
        if let Some(slot) = self.cache.get(xf as usize) {
            let id = slot.load(Ordering::Acquire);
            if id != u16::MAX {
                return id;
            }
        }
        self.resolve_slow(xf)
    }

    #[cold]
    fn resolve_slow(&self, xf: u16) -> u16 {
        let mut guard = self.inner.lock().unwrap();
        let (styles, ids) = &mut *guard;
        let id = *ids.entry(xf).or_insert_with(|| {
            let (cs, theme_xf, pattern, quote) = self.parsed.resolve_xf(xf as usize);
            let id = styles.intern(cs);
            if (xf as usize) < self.parsed.xf_count() {
                styles.set_theme(id, theme_xf);
            }
            if let Some(p) = pattern {
                styles.set_pattern(id, p);
            }
            if quote {
                styles.set_quote(id, true);
            }
            id
        });
        if let Some(slot) = self.cache.get(xf as usize) {
            slot.store(id, Ordering::Release);
        }
        id
    }

    pub(super) fn into_inner(self) -> (crate::style::StyleTable, HashMap<u16, u16>) {
        self.inner.into_inner().unwrap()
    }
}

#[derive(Default)]
struct Strings {
    date_system: crate::DateSystem,
    dynamic: std::collections::HashSet<u32>,
    text: TextPool,
    rich: HashMap<usize, Vec<RichRun>>,
}

// Values are decoded directly into `Arc<str>` entries so concurrent worksheet
// workers share string handles in O(1) with zero slab copies and zero atomics.
#[derive(Default)]
struct TextPool {
    items: Vec<Arc<str>>,
}

impl TextPool {
    fn reserve(&mut self, additional: usize) {
        self.items.reserve(additional);
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    fn push(&mut self, text: String) {
        self.items.push(Arc::from(text));
    }

    fn push_arc(&mut self, arc: Arc<str>) {
        self.items.push(arc);
    }

    fn extend_arcs(&mut self, iter: impl IntoIterator<Item = Arc<str>>) {
        self.items.extend(iter);
    }

    fn prepare_shared(&mut self) {}

    #[cfg(test)]
    fn get(&self, id: usize) -> Option<&str> {
        self.items.get(id).map(AsRef::as_ref)
    }

    fn get_arc(&self, id: usize) -> Option<&Arc<str>> {
        self.items.get(id)
    }
}

fn attr<'a>(e: &'a BytesStart<'a>, name: &str) -> Option<Cow<'a, str>> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .map(|a| a.value)
}

// ST_Xstring uses UTF-16 code-unit escapes; _x005F_x0041_ denotes the
// literal string "_x0041_". Decode once, including adjacent surrogate pairs.
fn office_text(s: &str) -> Cow<'_, str> {
    if stream::simd_memchr(b'_', s.as_bytes()).is_none() || !s.contains("_x") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let unit = |s: &str| {
        s.as_bytes()
            .get(..7)
            .filter(|b| b.starts_with(b"_x") && b[6] == b'_')
            .and_then(|b| std::str::from_utf8(&b[2..6]).ok())
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|s| u16::from_str_radix(s, 16).ok())
    };
    while let Some(pos) = rest.find("_x") {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        if let Some(u) = unit(rest) {
            rest = &rest[7..];
            let mut value = u32::from(u);
            if (0xd800..=0xdbff).contains(&u) {
                if let Some(low @ 0xdc00..=0xdfff) = unit(rest) {
                    value = 0x10000 + (value - 0xd800) * 1024 + u32::from(low) - 0xdc00;
                    rest = &rest[7..];
                }
            }
            out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
        } else {
            out.push('_');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn content<'a>(r: &mut Reader<&'a [u8]>) -> Result<Cow<'a, str>> {
    let mut text = Cow::Borrowed("");
    loop {
        let next = match r.read_event()? {
            Event::Text(t) if !t.as_ref().contains('\r') => t.into_inner(),
            Event::CData(t) if !AsRef::<str>::as_ref(&t).contains('\r') => t.into_inner(),
            Event::Text(t) => Cow::Owned(t.xml10_content().into_owned()),
            Event::CData(t) => Cow::Owned(t.xml10_content().into_owned()),
            Event::GeneralRef(e) => {
                let s = crate::model::resolve_entity(e.as_ref().as_ref());
                ensure!(!s.is_empty(), "无效 XML 实体");
                Cow::Owned(s)
            }
            Event::End(_) => return Ok(text),
            Event::Comment(_) => continue,
            _ => anyhow::bail!("元素文本未正常结束"),
        };
        if text.is_empty() {
            text = next;
        } else {
            text.to_mut().push_str(&next);
        }
    }
}

fn string_item(r: &mut Reader<&[u8]>) -> Result<(String, Vec<RichRun>)> {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut run: Option<RichRun> = None;
    let mut in_props = false;
    loop {
        let event = r.read_event()?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let tag = e.local_name();
                let empty = matches!(event, Event::Empty(_));
                match tag.as_ref() {
                    "r" if !empty => run = Some(RichRun::plain("")),
                    "rPr" if !empty => in_props = true,
                    "rPh" if !empty => {
                        r.read_to_end(e.name())?;
                    }
                    "t" if !empty => {
                        let value = content(r)?;
                        let preserve = attr(e, "xml:space").as_deref() == Some("preserve");
                        let value = office_text(if preserve {
                            &value
                        } else {
                            value.trim_matches([' ', '\t', '\r', '\n'])
                        });
                        text.push_str(&value);
                        if let Some(run) = &mut run {
                            run.text.push_str(&value);
                        }
                    }
                    prop if in_props => {
                        if let Some(run) = &mut run {
                            let val = attr(e, "val");
                            let on = !matches!(val.as_deref(), Some("0" | "false"));
                            match prop {
                                "b" => run.bold = on,
                                "i" => run.italic = on,
                                "rFont" => run.font = val.map(|v| crate::model::xml_unescape(&v)),
                                "sz" => run.size = val.and_then(|v| v.parse().ok()),
                                "color" => {
                                    run.color = attr(e, "rgb").and_then(|v| {
                                        let rgb = v.get(v.len().checked_sub(6)?..)?;
                                        let v = u32::from_str_radix(rgb, 16).ok()?;
                                        Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
                                    })
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "si" | "is" => {
                    if runs.iter().all(RichRun::is_plain) {
                        runs.clear();
                    }
                    return Ok((text, runs));
                }
                "r" => {
                    if let Some(run) = run.take() {
                        runs.push(run);
                    }
                }
                "rPr" => in_props = false,
                _ => {}
            },
            Event::Eof => anyhow::bail!("字符串元素未正常结束"),
            _ => {}
        }
    }
}

pub(super) fn xml_part<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    part: &str,
) -> Result<String> {
    let mut entry = zip
        .by_name(part)
        .with_context(|| format!("缺少部件 {part}"))?;
    let cap = if part.starts_with("xl/worksheets/") || part == "xl/sharedStrings.xml" {
        crate::fixups::MAX_SHEET_PART_BYTES
    } else {
        crate::fixups::MAX_META_PART_BYTES
    };
    let bytes = crate::fixups::read_zip_entry_limited(&mut entry, cap)?;
    decode_xml_part(bytes, part)
}

pub(super) fn utf16_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(b"<\0") {
        Some(encoding_rs::UTF_16LE)
    } else if bytes.starts_with(&[0xfe, 0xff]) || bytes.starts_with(b"\0<") {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

pub(super) fn decode_xml_part(bytes: Vec<u8>, part: &str) -> Result<String> {
    if let Some(encoding) = utf16_encoding(&bytes) {
        let (s, _, invalid) = encoding.decode(&bytes);
        ensure!(!invalid, "{part}: UTF-16 编码错误");
        let mut xml = s.into_owned();
        // Saving a modified part emits UTF-8 bytes. Its declaration must describe
        // those bytes even when the original XML was UTF-16LE/BE.
        if xml.starts_with("<?xml") {
            let mut reader = Reader::from_str(&xml);
            if let Event::Decl(decl) = reader.read_event()? {
                let version = decl.version()?;
                let standalone = decl.standalone().transpose()?;
                let decl = quick_xml::events::BytesDecl::new(
                    &version,
                    Some("UTF-8"),
                    standalone.as_deref(),
                );
                let mut writer = quick_xml::Writer::new(Vec::new());
                writer.write_event(Event::Decl(decl))?;
                let end = reader.buffer_position() as usize;
                xml.replace_range(..end, std::str::from_utf8(&writer.into_inner())?);
            }
        }
        return Ok(xml);
    }
    Ok(String::from_utf8(bytes).with_context(|| format!("{part}: 工作表 XML 编码无效"))?)
}

fn strings<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<Strings> {
    let mut table = Strings::default();
    if zip.index_for_name("xl/sharedStrings.xml").is_none() {
        return Ok(table);
    }
    let part = "xl/sharedStrings.xml";
    super::delta::stream::xml_input(zip.by_name(part)?, part, |input| {
        stream::string_table(input, &mut table)
    })?;
    table.text.prepare_shared();
    Ok(table)
}

#[derive(Default)]
pub(super) struct RowLayout {
    pub row_heights: Vec<(u32, f32)>,
    pub hidden_rows: Vec<u32>,
    pub row_levels: Vec<(u32, u8)>,
    pub row_xfs: Vec<(u32, u32)>,
}

impl RowLayout {
    pub(super) fn extend(&mut self, other: Self) {
        self.row_heights.extend(other.row_heights);
        self.hidden_rows.extend(other.hidden_rows);
        self.row_levels.extend(other.row_levels);
        self.row_xfs.extend(other.row_xfs);
    }

    fn apply_to(&mut self, layout: &mut crate::merges::SheetLayout) {
        layout.row_heights.append(&mut self.row_heights);
        layout.hidden_rows.append(&mut self.hidden_rows);
        layout.row_levels.append(&mut self.row_levels);
        layout.row_xfs.append(&mut self.row_xfs);
    }
}

pub(super) struct Loaded {
    pub sheet: Sheet,
    pub xfs: Vec<(u32, u32, u16)>,
    pub row_layout: RowLayout,
    pub layout: Option<crate::merges::SheetLayout>,
    pub diag: LoadDiag,
    pub all_rows_clean: bool,
    pub clean_shell: Option<Arc<str>>,
    pub sst_arcs: Option<Arc<[Arc<str>]>>,
}

type MasterSpec = ((u32, u32), (u32, u32, u32, u32), String);

#[derive(Default)]
pub(super) struct SharedMasters {
    map: HashMap<u32, MasterSpec>,
}

#[derive(Default)]
struct Formulas {
    masters: HashMap<u32, MasterSpec>,
    children: Vec<((u32, u32), u32)>,
}

fn formula_range(s: &str) -> Result<(u32, u32, u32, u32)> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let coord = |s: &str| {
        if s.contains('$') {
            crate::sparse::parse_ref(&s.replace('$', ""))
        } else {
            crate::sparse::parse_ref(s)
        }
        .filter(|&(r, c)| r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS)
    };
    let (r, c) = coord(a).context("公式区域起点无效")?;
    let (rr, cc) = coord(b).context("公式区域终点无效")?;
    Ok((r.min(rr), c.min(cc), r.max(rr), c.max(cc)))
}

#[inline]
fn translate_shared_child(
    master_entry: &MasterSpec,
    pos: (u32, u32),
    sheet_name: &str,
) -> Result<Arc<str>> {
    let ((mr, mc), rect, master) = master_entry;
    ensure!(
        (rect.0..=rect.2).contains(&pos.0) && (rect.1..=rect.3).contains(&pos.1),
        "共享公式子单元格在声明范围外"
    );
    let translated = crate::refs::translate(
        master,
        i64::from(pos.0) - i64::from(*mr),
        i64::from(pos.1) - i64::from(*mc),
        sheet_name,
    );
    let body = translated.trim_start_matches('=');
    let mut with_eq = String::with_capacity(body.len() + 1);
    with_eq.push('=');
    with_eq.push_str(body);
    Ok(crate::normalize_formula(&with_eq).into())
}

fn cell(
    r: &mut Reader<&[u8]>,
    dst: &mut Loaded,
    table: &Strings,
    formulas: &mut Formulas,
    shared_masters: Option<&RwLock<SharedMasters>>,
    pos: (u32, u32),
    typ: &str,
    dynamic: bool,
) -> Result<Cell> {
    let mut value = Value::Empty;
    let mut formula = None;
    loop {
        let ev = r.read_event()?;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "v" => {
                        let v = if empty {
                            Cow::Borrowed("")
                        } else {
                            content(r)?
                        };
                        value = match typ {
                            "s" if !v.is_empty() => {
                                let id: usize = v.trim().parse().context("无效共享字符串序号")?;
                                if let Some(runs) = table.rich.get(&id) {
                                    dst.sheet.rich.insert(pos, runs.clone());
                                }
                                let arc = table.text.get_arc(id).context("缺少共享字符串")?;
                                shared_text_value(arc)
                            }
                            "inlineStr" | "is" => continue,
                            "str" => Value::Text(office_text(&v).as_ref().into()),
                            "d" if !v.is_empty() => super::iso_date_value(&v, table.date_system),
                            _ if v.is_empty() => Value::Empty,
                            "b" => match v.trim() {
                                "" => Value::Empty,
                                v if v == "1" || v.eq_ignore_ascii_case("true") => {
                                    Value::Bool(true)
                                }
                                v if v == "0" || v.eq_ignore_ascii_case("false") => {
                                    Value::Bool(false)
                                }
                                _ => anyhow::bail!("无效布尔值 {v}"),
                            },
                            "e" => {
                                if v == "#SPILL!" {
                                    Value::Error(crate::formula::ErrKind::Spill)
                                } else {
                                    super::data_to_value(&calamine::Data::Error(
                                        v.parse().map_err(|_| anyhow::anyhow!("无效错误值 {v}"))?,
                                    ), table.date_system)
                                }
                            }
                            "" => fast_parse_f64(&v)
                                .map(Value::Number)
                                .unwrap_or_else(|_| Value::Text(v.as_ref().into())),
                            "n" => Value::Number(fast_parse_f64(&v)?),
                            _ => anyhow::bail!("无法识别的单元格类型 {typ}"),
                        };
                    }
                    "is" => {
                        let (text, runs) = if empty {
                            Default::default()
                        } else {
                            string_item(r)?
                        };
                        value = Value::Text(text.into());
                        if !runs.is_empty() {
                            dst.sheet.rich.insert(pos, runs);
                        }
                    }
                    "f" => {
                        let f = if empty {
                            Cow::Borrowed("")
                        } else {
                            content(r)?
                        };
                        match attr(e, "t").as_deref() {
                            Some("shared") => {
                                let si = attr(e, "si").context("共享公式没有 si")?.parse()?;
                                if let Some(range) = attr(e, "ref") {
                                    let rect = formula_range(&range)?;
                                    ensure!(
                                        !f.is_empty()
                                            && (rect.0..=rect.2).contains(&pos.0)
                                            && (rect.1..=rect.3).contains(&pos.1),
                                        "共享公式主单元格无效"
                                    );
                                    let spec = (pos, rect, f.to_string());
                                    if let Some(global) = shared_masters {
                                        global.write().unwrap().map.entry(si).or_insert_with(|| spec.clone());
                                    }
                                    ensure!(
                                        formulas.masters.insert(si, spec).is_none(),
                                        "共享公式序号重复"
                                    );
                                } else {
                                    if let Some(master_entry) = formulas.masters.get(&si) {
                                        formula = Some(translate_shared_child(
                                            master_entry,
                                            pos,
                                            &dst.sheet.name,
                                        )?);
                                        continue;
                                    }
                                    if let Some(global) = shared_masters {
                                        if let Some(master_entry) = global.read().unwrap().map.get(&si) {
                                            formula = Some(translate_shared_child(
                                                master_entry,
                                                pos,
                                                &dst.sheet.name,
                                            )?);
                                            continue;
                                        }
                                    }
                                    formulas.children.push((pos, si));
                                    continue;
                                }
                            }
                            Some("array") => {
                                let range = attr(e, "ref").context("数组公式缺少区域")?;
                                let rect = formula_range(&range)?;
                                let anchor = CellRef::new(pos.0, pos.1);
                                if dynamic {
                                    ensure!((rect.0, rect.1) == pos, "动态数组锚点与范围不一致");
                                    dst.sheet
                                        .spill_shapes
                                        .insert(anchor, (rect.2 - rect.0 + 1, rect.3 - rect.1 + 1));
                                } else {
                                    dst.sheet.arrays.push((anchor, rect));
                                }
                            }
                            Some("dataTable") => {
                                if !dst.diag.problems.iter().any(|p| p.what == "模拟运算表公式")
                                {
                                    dst.diag.push(
                                        Some(&dst.sheet.name),
                                        dst.sheet.source_part.as_deref().unwrap_or("xl/worksheets"),
                                        "模拟运算表公式",
                                        format!(
                                            "{} 的模拟运算表参数无法完整重建，保留缓存值",
                                            crate::model::cell_name(pos.0, pos.1)
                                        ),
                                        true,
                                    );
                                }
                            }
                            Some("normal") | None => {}
                            Some(typ) => anyhow::bail!("未知公式类型 {typ}"),
                        }
                        if !f.is_empty() {
                            let body = f.trim_start_matches('=');
                            let mut with_eq = String::with_capacity(body.len() + 1);
                            with_eq.push('=');
                            with_eq.push_str(body);
                            formula = Some(crate::normalize_formula(&with_eq).into());
                        }
                    }
                    _ if !empty => {
                        r.read_to_end(e.name())?;
                    }
                    _ => {}
                }
            }
            Event::End(e) if e.local_name().as_ref() == "c" => break,
            Event::Eof => anyhow::bail!("单元格未正常结束"),
            _ => {}
        }
    }
    Ok(Cell { value, formula })
}

fn parse(
    xml: &str,
    table: &Strings,
    style_lookup: Option<&StyleLookup<'_>>,
    dst: &mut Loaded,
    typ: calamine::SheetType,
) -> Result<String> {
    let mut f = Formulas::default();
    let shell = parse_section(xml, table, style_lookup, &[], None, dst, typ, &mut f, None, 0)?;
    expand_shared(dst, f)?;
    Ok(shell)
}

type RowCells = Vec<((u32, u32), Cell)>;
type RowStyles = Vec<(CellRef, u16)>;

#[allow(clippy::too_many_arguments)]
fn parse_section(
    xml: &str,
    table: &Strings,
    style_lookup: Option<&StyleLookup<'_>>,
    col_has_xf_hint: &[bool],
    shared_masters: Option<&RwLock<SharedMasters>>,
    dst: &mut Loaded,
    typ: calamine::SheetType,
    f: &mut Formulas,
    mut batch_out: Option<(&mut RowCells, &mut RowStyles)>,
    first_row: u32,
) -> Result<String> {
    let mut r = Reader::from_str(xml);
    let mut pos = (first_row, 0u32);
    let fragment = batch_out.is_some();
    let mut in_data = fragment;
    let mut local_col_has_xf: Vec<bool> = Vec::new();
    let mut row_has_xf = false;
    let expected_root = match typ {
        calamine::SheetType::WorkSheet => "worksheet",
        calamine::SheetType::ChartSheet => "chartsheet",
        calamine::SheetType::DialogSheet => "dialogsheet",
        calamine::SheetType::MacroSheet => "macrosheet",
        calamine::SheetType::Vba => "module",
    };
    let initial_depth: usize = if fragment { 2 } else { 0 };
    let mut depth = initial_depth;
    let mut saw_data = fragment;
    let mut closed = false;
    let mut shell = String::new();
    let mut copied = 0;
    loop {
        let start = r.buffer_position() as usize;
        let event = r.read_event()?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                let tag = e.local_name();
                let level = depth;
                if fragment && level == initial_depth {
                    ensure!(tag.as_ref() == "row", "行块含其他元素");
                }
                if level == 0 {
                    ensure!(
                        !closed && tag.as_ref() == expected_root,
                        "部件根元素应为 {expected_root}"
                    );
                    closed = empty;
                }
                if !empty {
                    depth += 1;
                }
                match tag.as_ref() {
                    "col" if !in_data => {
                        let mut min = 0u32;
                        let mut max = 0u32;
                        let mut style_xf = 0u32;
                        for a in e.attributes().flatten() {
                            match a.key.as_ref() {
                                k if k.ends_with("min") => min = a.value.parse().unwrap_or(0),
                                k if k.ends_with("max") => max = a.value.parse().unwrap_or(0),
                                "style" => style_xf = a.value.parse().unwrap_or(0),
                                _ => {}
                            }
                        }
                        if style_xf > 0 && min >= 1 && max >= min {
                            let end = max.min(16_384) as usize;
                            if local_col_has_xf.len() < end {
                                local_col_has_xf.resize(end, false);
                            }
                            for c in (min as usize - 1)..end {
                                local_col_has_xf[c] = true;
                            }
                        }
                    }
                    "sheetData" if level == 1 => {
                        ensure!(!saw_data, "重复的 sheetData");
                        saw_data = true;
                        in_data = !empty;
                    }
                    "row" if in_data && level == 2 => {
                        let mut explicit_row = false;
                        let mut ht = 0f32;
                        let mut custom_ht = false;
                        let mut hidden = false;
                        let mut row_s = 0u32;
                        let mut row_lv = 0u8;
                        for a in e.attributes() {
                            let a = a?;
                            match a.key.as_ref() {
                                "r" => {
                                    pos.0 = a
                                        .value
                                        .parse::<u32>()?
                                        .checked_sub(1)
                                        .context("行号为零")?;
                                    explicit_row = true;
                                }
                                "ht" => ht = a.value.parse().unwrap_or(0.0),
                                "customHeight" => {
                                    custom_ht = a.value == "1" || a.value == "true";
                                }
                                "hidden" => {
                                    hidden = a.value == "1" || a.value == "true";
                                }
                                "s" => row_s = a.value.parse().unwrap_or(0),
                                "outlineLevel" => row_lv = a.value.parse::<u8>().unwrap_or(0),
                                _ => {}
                            }
                        }
                        row_has_xf = row_s > 0;
                        if row_lv > 0 {
                            dst.row_layout.row_levels.push((pos.0, row_lv));
                        }
                        if row_s > 0 {
                            dst.row_layout.row_xfs.push((pos.0, row_s));
                        }
                        if hidden {
                            dst.row_layout.hidden_rows.push(pos.0);
                        }
                        if ht > 0.0 && custom_ht {
                            let px = crate::layout::pt_to_px(ht);
                            if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
                                dst.row_layout.row_heights.push((pos.0, px));
                            }
                        }
                        dst.sheet.source_needs_dom_repair |= !explicit_row;
                        // Extract row metadata in-flight and omit `<row>` tags from `shell`
                        // so downstream layout/CF/DV/page scanners never re-parse millions of rows.
                        shell.push_str(&xml[copied..start]);
                        copied = r.buffer_position() as usize;
                        pos.1 = 0;
                        if empty {
                            pos.0 = pos.0.saturating_add(1);
                        }
                    }
                    "c" if in_data && level == 3 => {
                        let mut typ = Cow::Borrowed("");
                        let mut xf = None;
                        let mut dynamic = false;
                        let mut here = pos;
                        let mut explicit_cell = false;
                        for a in e.attributes() {
                            let a = a?;
                            match a.key.as_ref() {
                                "r" => {
                                    explicit_cell = true;
                                    here = crate::sparse::parse_ref(&a.value)
                                        .context("无效单元格坐标")?
                                }
                                "s" => xf = Some(a.value.parse::<u16>()?),
                                "cm" => dynamic = table.dynamic.contains(&a.value.parse::<u32>()?),
                                "t" => typ = a.value,
                                _ => {}
                            }
                        }
                        dst.sheet.source_needs_dom_repair |= !explicit_cell;
                        ensure!(
                            here.0 < crate::sync::MAX_ROWS && here.1 < crate::sync::MAX_COLS,
                            "单元格坐标越界"
                        );
                        if let Some(raw_xf) = xf {
                            if let Some(lookup) = style_lookup {
                                let id = lookup.resolve(raw_xf);
                                let col_styled = col_has_xf_hint
                                    .get(here.1 as usize)
                                    .copied()
                                    .unwrap_or(false)
                                    || local_col_has_xf
                                        .get(here.1 as usize)
                                        .copied()
                                        .unwrap_or(false);
                                if raw_xf != 0 || id != 0 || row_has_xf || col_styled {
                                    let key = CellRef::new(here.0, here.1);
                                    if let Some((_, ref mut styles)) = batch_out {
                                        styles.push((key, id));
                                    } else {
                                        dst.sheet.styles.insert(key, id);
                                    }
                                }
                            } else {
                                dst.xfs.push((here.0, here.1, raw_xf));
                            }
                        }
                        if !empty {
                            let cell = cell(
                                &mut r,
                                dst,
                                table,
                                f,
                                shared_masters,
                                here,
                                &typ,
                                dynamic,
                            )?;
                            if !cell.is_blank() {
                                if let Some((ref mut cells, _)) = batch_out {
                                    cells.push((here, cell));
                                } else {
                                    dst.sheet.set(here.0, here.1, cell);
                                }
                            }
                            depth -= 1;
                        }
                        pos.1 = here.1 + 1;
                        shell.push_str(&xml[copied..start]);
                        copied = r.buffer_position() as usize;
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let tag = e.local_name();
                depth = depth.checked_sub(1).context("工作表 XML 标签不匹配")?;
                if depth == 0 {
                    closed = true;
                }
                if depth == 1 && tag.as_ref() == "sheetData" {
                    in_data = false;
                }
                if depth == 2 && in_data && tag.as_ref() == "row" {
                    shell.push_str(&xml[copied..start]);
                    copied = r.buffer_position() as usize;
                    pos.0 = pos.0.saturating_add(1);
                    pos.1 = 0;
                    row_has_xf = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!((fragment || closed) && depth == initial_depth, "工作表 XML 未正常结束");
    ensure!(
        saw_data || typ != calamine::SheetType::WorkSheet,
        "工作表缺少 sheetData"
    );
    shell.push_str(&xml[copied..]);
    Ok(shell)
}

fn expand_shared(dst: &mut Loaded, f: Formulas) -> Result<()> {
    for ((r, c), si) in f.children {
        let master_entry = f.masters.get(&si).context("共享公式缺少主单元格")?;
        let formula = translate_shared_child(master_entry, (r, c), &dst.sheet.name)?;
        if let Some(existing) = dst.sheet.get_mut(r, c) {
            existing.formula = Some(formula);
        } else {
            dst.sheet.set(
                r,
                c,
                Cell {
                    value: Value::Empty,
                    formula: Some(formula),
                },
            );
        }
    }
    Ok(())
}

impl Loaded {
    fn empty(name: &str, part: &str) -> Self {
        let mut sheet = Sheet::new(name);
        sheet.source_part = Some(part.to_owned());
        sheet.loading = true;
        Self {
            sheet,
            xfs: Vec::new(),
            row_layout: RowLayout::default(),
            layout: None,
            diag: Default::default(),
            all_rows_clean: true,
            clean_shell: None,
            sst_arcs: None,
        }
    }
}

struct ProgressCancelReader<'a, R> {
    inner: R,
    ctrl: &'a super::LoadControl,
}

impl<R: std::io::Read> std::io::Read for ProgressCancelReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.ctrl.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                super::LOAD_CANCELLED_ERR,
            ));
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.ctrl
                .done
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(n)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn worksheets<'a>(
    path_for_rels: Option<&Path>,
    zip: &mut zip::ZipArchive<Cursor<&'a [u8]>>,
    header: &Header,
    targets: &HashMap<String, String>,
    style_lookup: &StyleLookup<'_>,
    dxfs: &[crate::cf::Dxf],
    dxf_ext: bool,
    has_drawing: bool,
    ctrl: Option<&super::LoadControl>,
) -> Result<Vec<Loaded>> {
    if ctrl.is_some_and(|c| c.is_cancelled()) {
        anyhow::bail!(super::LOAD_CANCELLED_ERR);
    }
    let names = &header.names;
    let mut shared = strings(zip).context("读取 xl/sharedStrings.xml 失败")?;
    shared.dynamic.clone_from(&header.dynamic);
    shared.date_system = header.date_system;
    let signatures: Vec<_> = names
        .iter()
        .map(|n| targets.get(n).and_then(|p| signature(zip, p)))
        .collect();
    if let Some(c) = ctrl {
        let total_bytes: u64 = signatures.iter().filter_map(|s| s.map(|(_, sz)| sz)).sum();
        c.total.store(total_bytes.max(1), Ordering::Relaxed);
    }
    // Streaming worksheets (>= 1 MiB) use bounded row chunks rather than whole-sheet buffers,
    // so only buffered sheets (< 1 MiB) count toward the uncompressed memory cap.
    let largest_buffered = signatures
        .iter()
        .enumerate()
        .filter_map(|(idx, sig)| {
            let (_, size) = (*sig)?;
            let streamed = header.types.get(idx) == Some(&calamine::SheetType::WorkSheet)
                && size >= 1024 * 1024;
            (!streamed).then_some(size)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let cores = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let workers = cores
        .min(names.len())
        .min((128_000_000 / largest_buffered).max(1) as usize)
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let run = |zip: &mut zip::ZipArchive<Cursor<&'a [u8]>>| -> Result<Vec<(usize, Loaded)>> {
        let mut sheets = Vec::new();
        loop {
            if ctrl.is_some_and(|c| c.is_cancelled()) {
                anyhow::bail!(super::LOAD_CANCELLED_ERR);
            }
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(name) = names.get(index) else { break };
            let part = targets.get(name).context("工作表缺少部件路径")?;
            ensure!(
                signature(zip, part) == signatures[index],
                "读取期间工作表 {name} 已被替换，请重新打开"
            );
            let mut loaded = Loaded::empty(name, part);
            let typ = header.types[index];
            let sheet_size = signatures[index].map_or(0, |(_, sz)| sz);
            let parsed = if typ == calamine::SheetType::WorkSheet && sheet_size >= 1024 * 1024 {
                let entry = zip.by_name(part)?;
                let mut stream_workers = (cores / workers).max(1);
                if stream_workers == 1 && cores >= 2 && sheet_size >= 4 * 1024 * 1024 {
                    stream_workers = 2;
                }
                if let Some(c) = ctrl {
                    let tracked = ProgressCancelReader {
                        inner: entry,
                        ctrl: c,
                    };
                    super::delta::stream::xml_input(tracked, part, |input| {
                        stream::worksheet_with_styles(
                            input,
                            &shared,
                            Some(style_lookup),
                            &mut loaded,
                            stream_workers,
                            128 * 1024,
                        )
                    })
                } else {
                    super::delta::stream::xml_input(entry, part, |input| {
                        stream::worksheet_with_styles(
                            input,
                            &shared,
                            Some(style_lookup),
                            &mut loaded,
                            stream_workers,
                            128 * 1024,
                        )
                    })
                }
            } else {
                let res = xml_part(zip, part)
                    .and_then(|xml| parse(&xml, &shared, Some(style_lookup), &mut loaded, typ));
                if let Some(c) = ctrl {
                    c.done.fetch_add(sheet_size, Ordering::Relaxed);
                }
                res
            };
            match parsed {
                Ok(shell) => {
                    for (anchor, (h, w)) in loaded.sheet.spill_shapes.clone() {
                        if loaded.sheet.value(anchor.row, anchor.col)
                            == Value::Error(crate::formula::ErrKind::Spill)
                        {
                            loaded.sheet.spill_shapes.insert(anchor, (0, 0));
                        } else {
                            let tails: Vec<_> = loaded
                                .sheet
                                .iter_rect((
                                    anchor.row,
                                    anchor.col,
                                    anchor.row + h - 1,
                                    anchor.col + w - 1,
                                ))
                                .filter_map(|(k, c)| {
                                    (k != anchor && c.formula.is_none()).then_some(k)
                                })
                                .collect();
                            loaded.sheet.spilled.extend(tails);
                        }
                    }
                    let mut layout = crate::merges::sheet_layout(&shell, dxfs, dxf_ext);
                    loaded.row_layout.apply_to(&mut layout);
                    loaded.layout = Some(layout);
                    loaded.sheet.page = crate::page::parse_page_setup(&shell);
                    if has_drawing && shell.contains("drawing") {
                        if let Some(p) = path_for_rels {
                            loaded.sheet.textboxes =
                                crate::textbox::load_from_sheet(p, part, &shell);
                        }
                    }
                    if shell.contains("hyperlink") {
                        loaded.sheet.hyperlinks = crate::hyperlinks::load_sheet(
                            path_for_rels.unwrap_or(Path::new("")),
                            part,
                            &shell,
                        )
                        .into();
                    }
                    if shell.contains("sparkline") {
                        loaded.sheet.sparklines = crate::sparkline::parse_sheet(&shell);
                        loaded.sheet.sparklines.sort_by_key(|s| (s.c, s.r));
                    }
                }
                Err(e) => loaded.diag.push(
                    Some(name),
                    part,
                    "稀疏单元格数据、公式和样式",
                    format!("{e:#}"),
                    true,
                ),
            }
            sheets.push((index, loaded));
        }
        Ok(sheets)
    };
    let mut out_sheets: Vec<Loaded> = std::thread::scope(|scope| -> Result<Vec<Loaded>> {
        let mut jobs = Vec::new();
        for _ in 1..workers {
            let mut worker_zip = zip.clone();
            jobs.push(scope.spawn(move || run(&mut worker_zip)));
        }
        let mut sheets = run(zip)?;
        for job in jobs {
            sheets.extend(
                job.join()
                    .map_err(|_| anyhow::anyhow!("读取工作表线程异常退出"))??,
            );
        }
        sheets.sort_unstable_by_key(|(i, _)| *i);
        Ok(sheets.into_iter().map(|(_, s)| s).collect())
    })?;
    if out_sheets.iter().any(|s| s.clean_shell.is_some())
        && shared.rich.is_empty()
        && !shared.text.items.is_empty()
    {
        let sst_arcs: Arc<[Arc<str>]> = Arc::from(shared.text.items);
        for s in &mut out_sheets {
            if s.clean_shell.is_some() {
                s.sst_arcs = Some(Arc::clone(&sst_arcs));
            }
        }
    }
    Ok(out_sheets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DateSystem, Workbook};

    #[test]
    fn parallel_row_chunks_match_serial_layout_shared_formulas_rich_text_and_caches() {
        let mut xml = String::from("<worksheet><sheetPr/><sheetData>");
        for row in 1..=80 {
            let f = if row == 1 {
                r#"<f t="shared" si="1" ref="B1:B80">A1+2</f>"#
            } else {
                r#"<f t="shared" si="1"/>"#
            };
            xml.push_str(&format!(
                r#"<row r="{row}" ht="24" customHeight="1"><c r="A{row}" s="2"><v>{row}</v></c><c r="B{row}">{f}<v>9</v></c><c t="inlineStr"><is><r><rPr><b/></rPr><t>字</t></r></is></c></row>"#
            ));
        }
        xml.push_str("</sheetData><extLst/></worksheet>");
        let table = Strings::default();
        let typ = calamine::SheetType::WorkSheet;
        let mut serial = Loaded::empty("Data", "sheet");
        let shell = parse(&xml, &table, None, &mut serial, typ).unwrap();
        let mut parallel = Loaded::empty("Data", "sheet");
        let row_shell = stream::worksheet(xml.as_bytes(), &table, &mut parallel, 3, 1100).unwrap();
        assert_eq!(row_shell, shell);
        assert_eq!(parallel.xfs, serial.xfs);
        assert_eq!(parallel.row_layout.row_heights, serial.row_layout.row_heights);
        assert_eq!(parallel.row_layout.row_heights.len(), 80);
        assert_eq!(parallel.sheet.rich, serial.sheet.rich);
        assert_eq!(
            parallel.sheet.iter().collect::<Vec<_>>(),
            serial.sheet.iter().collect::<Vec<_>>()
        );
        assert_eq!(parallel.sheet.get(79, 1).unwrap().formula.as_deref(), Some("=A80+2"));
        let commented = xml.replace("<sheetData>", "<sheetData><!-- <row r='200'> -->");
        let mut comments = Loaded::empty("Data", "sheet");
        let kept = stream::worksheet(commented.as_bytes(), &table, &mut comments, 3, 1100).unwrap();
        assert!(kept.contains("<!-- <row r='200'> -->"));
        let broken = xml.replace("ref=\"B1:B80\"", "ref=\"B1:B3\"");
        assert!(stream::worksheet(broken.as_bytes(), &table, &mut Loaded::empty("Data", "sheet"), 3, 1100).is_err());
        assert!(parse(&broken, &table, None, &mut Loaded::empty("Data", "sheet"), typ).is_err());
    }

    #[test]
    fn opened_utf16_sheet_streams_implicit_rows_across_batches_and_resolves_late_masters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.xlsx");
        super::super::save_atomic(&path, &Workbook::new_blank()).unwrap();
        let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-16"?><s:worksheet xmlns:s="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><s:sheetData>"#);
        let filler = "文".repeat(1100);
        for row in 1..=550 {
            let f = if row == 550 {
                r#"<s:f t="shared" si="3" ref="B1:B550">A550+1</s:f>"#
            } else {
                r#"<s:f t="shared" si="3"/>"#
            };
            xml.push_str(&format!(
                r#"<s:row ht="23"><s:c><s:v>{row}</s:v></s:c><s:c>{f}<s:v>1</s:v></s:c><s:c t="inlineStr"><s:is><s:t>{filler}</s:t></s:is></s:c></s:row>"#
            ));
        }
        xml.push_str("</s:sheetData></s:worksheet>");
        let bytes: Vec<_> = [0xfe, 0xff]
            .into_iter()
            .chain(xml.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        crate::fixups::write_parts(&path, &[("xl/worksheets/sheet1.xml".into(), bytes)]).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        let sheet = wb.sheet(0);
        for row in [0, 202, 549] {
            assert_eq!(sheet.value(row, 0), Value::Number((row + 1) as f64));
            assert_eq!(
                sheet.get(row, 1).unwrap().formula.as_deref().unwrap(),
                format!("=A{}+1", row + 1)
            );
            assert_eq!(sheet.value(row, 2), Value::Text(filler.as_str().into()));
        }
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("months.xlsx");
        let mut wb = Workbook::new_blank();
        wb.date_system = DateSystem::D1904;
        wb.sheets.clear();
        let mut date = wb.styles.get(0).clone();
        date.format = "yyyy-mm-dd".into();
        let id = wb.styles.intern(date);
        for i in 1..=12 {
            let mut sh = Sheet::new(format!("Month {i:02}"));
            sh.set(
                0,
                1,
                Cell {
                    value: Value::Text("Amount".into()),
                    formula: None,
                },
            );
            sh.set(
                1,
                0,
                Cell {
                    value: Value::Number(1.),
                    formula: None,
                },
            );
            sh.set_style(1, 0, id);
            sh.set(
                1,
                1,
                Cell {
                    value: Value::Number(i as f64),
                    formula: None,
                },
            );
            wb.sheets.push(sh);
        }
        super::super::save(&path, &wb).unwrap();
        (dir, path)
    }

    #[test]
    fn indexed_strings_runs_forward_shared_formulas_implicit_cells_and_utf16() {
        let (_dir, path) = fixture();
        let parts = crate::patch::sheet_targets(&path).unwrap();
        let sst = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si/><si><r><rPr><b/></rPr><t>同文</t></r><rPh><t>phonetic</t></rPh></si><si><t>同文</t></si><si><r><rPr><i/></rPr><t>同文</t></r></si></sst>"#;
        let sheet = r#"<x:worksheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><x:sheetData><x:row r="4"><x:c t="s"><x:v>1</x:v></x:c><x:c t="s"><x:v>2</x:v></x:c><x:c t="s"><x:v>3</x:v></x:c><x:c t="inlineStr"><x:is><x:r><x:rPr><x:b/></x:rPr><x:t xml:space="preserve"> A&amp;B&#x20;_xD83D__xDE00_ </x:t></x:r></x:is></x:c><x:c t="d"><x:v>2026-06-01</x:v></x:c></x:row><x:row/><x:row hidden="1"><x:c/><x:c><x:f t="shared" si="0"/><x:v>12</x:v></x:c></x:row><x:row><x:c r="B7"><x:f t="shared" si="0" ref="B6:B7">A7+$D$2</x:f><x:v>13</x:v></x:c><x:c t="e"><x:v>#REF!</x:v></x:c><x:c t="b"><x:v> True </x:v></x:c><x:c><x:f t="array" ref="E7:E8">A7*2</x:f><x:v>4</x:v></x:c></x:row></x:sheetData></x:worksheet>"#;
        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(sheet.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        crate::fixups::write_parts(
            &path,
            &[
                ("xl/sharedStrings.xml".into(), sst.as_bytes().to_vec()),
                (parts["Month 10"].clone(), utf16),
            ],
        )
        .unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        let sh = &wb.sheets[9];
        assert_eq!(wb.date_system, DateSystem::D1904);
        assert_eq!(sh.value(3, 0), Value::Text("同文".into()));
        assert!(sh.rich[&(3, 0)][0].bold);
        assert!(!sh.rich.contains_key(&(3, 1)));
        assert!(sh.rich[&(3, 2)][0].italic);
        assert_eq!(sh.rich[&(3, 3)][0].text, " A&B 😀 ");
        assert_eq!(sh.value(3, 4), Value::Number(44_712.));
        assert_eq!(sh.get(5, 1).unwrap().formula.as_deref(), Some("=A6+$D$2"));
        assert_eq!(sh.value(5, 1), Value::Number(12.));
        assert_eq!(sh.value(6, 2), Value::Error(crate::formula::ErrKind::Ref));
        assert_eq!(sh.value(6, 3), Value::Bool(true));
        assert!(sh.arrays.contains(&(CellRef::new(6, 4), (6, 4, 7, 4))));
        assert!(sh.hidden_rows_user.contains(&5));
    }

    #[test]
    fn rejects_mismatched_parts_and_guards_unsupported_what_if_metadata() {
        let (_dir, path) = fixture();
        let targets = crate::patch::sheet_targets(&path).unwrap();
        let cases = [
            ("Month 01", "<other/>"),
            ("Month 02", "<worksheet/>"),
            (
                "Month 03",
                "<worksheet><sheetData/></worksheet><worksheet><sheetData/></worksheet>",
            ),
            (
                "Month 04",
                r#"<worksheet><sheetData><row><c><f t="dataTable" ref="A1:B2" r1="B3"/><v>22</v></c><c t="str"><v/></c><c t="str"><v></v></c></row></sheetData></worksheet>"#,
            ),
            (
                "Month 05",
                r#"<worksheet><sheetData><row><c><f t="shared" si="1" ref="A1:A1">A2</f></c><c><f t="shared" si="1"/></c></row></sheetData></worksheet>"#,
            ),
        ];
        let parts: Vec<_> = cases
            .iter()
            .map(|(name, xml)| (targets[*name].clone(), xml.as_bytes().to_vec()))
            .collect();
        crate::fixups::write_parts(&path, &parts).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        for (name, _) in cases {
            assert!(
                diag.problems
                    .iter()
                    .any(|p| p.fatal && p.sheet.as_deref() == Some(name)),
                "{name} {:?}",
                diag.problems
            );
        }
        let sh = &wb.sheets[3];
        assert_eq!(sh.value(0, 0), Value::Number(22.));
        assert_eq!(sh.value(0, 1), Value::Text("".into()));
        assert_eq!(sh.value(0, 2), Value::Text("".into()));
        assert!(wb.load_incomplete);
    }

    #[test]
    fn editing_utf16_sheet_and_shared_strings_transcodes_both_declarations_in_place() {
        let (_dir, path) = fixture();
        let target = crate::patch::sheet_targets(&path).unwrap()["Month 10"].clone();
        let sst = r#"<?xml version="1.0" encoding="UTF-16" standalone="no"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="1"><si><t>原值</t></si></sst>"#;
        let sheet = r#"<?xml version="1.0" encoding="UTF-16LE" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        let source_le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(sheet.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let shared_be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain(sst.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        crate::fixups::write_parts(
            &path,
            &[
                (target.clone(), source_le),
                ("xl/sharedStrings.xml".into(), shared_be),
            ],
        )
        .unwrap();
        let (mut book, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert_eq!(book.sheets[9].value(0, 1), Value::Text("原值".into()));
        book.sheets[9].set(
            0,
            0,
            Cell {
                value: Value::Text("改值 & 中".into()),
                formula: None,
            },
        );
        book.dirty = true;
        super::super::save(&path, &book).unwrap();
        for (part, standalone) in [(target.as_str(), "yes"), ("xl/sharedStrings.xml", "no")] {
            let raw = crate::fixups::read_part_file(&path, part).unwrap();
            let xml = String::from_utf8(raw).unwrap();
            let decl = xml.split_once("?>").unwrap().0;
            assert!(decl.contains(r#"encoding="UTF-8""#), "{part} {decl}");
            assert!(
                decl.contains(&format!(r#"standalone="{standalone}""#)),
                "{part} {decl}"
            );
        }
        let (again, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert_eq!(again.sheets[9].value(0, 0), Value::Text("改值 & 中".into()));
        assert_eq!(again.sheets[9].value(0, 1), Value::Text("原值".into()));
    }

    #[test]
    fn twelfth_sheet_chart_uses_its_own_values_and_workbook_calendar_without_cached_series() {
        let (_dir, path) = fixture();
        let wb = super::super::load(&path).unwrap();
        let sh = &wb.sheets[11];
        let mut chart = crate::chart_write::chart_from_selection(
            sh,
            &sh.name,
            (0, 0, 1, 1),
            "December",
            crate::charts::ChartKind::Column,
        );
        chart.series[0].cats.clear();
        chart.series[0].vals.clear();
        crate::chart_write::insert_chart(&path, &sh.name, &chart).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert!(wb.sheets[..11].iter().all(|s| s.charts.is_empty()));
        let series = &wb.sheets[11].charts[0].series[0];
        assert_eq!(series.vals, vec![12.]);
        assert_eq!(series.cats, vec!["1904-01-02"]);
    }

    #[test]
    fn single_pass_styles_matches_calamine_theme_and_ext() {
        use calamine::Reader as _;
        use std::io::{Cursor, Write};
        let theme_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office">
  <a:themeElements>
    <a:clrScheme name="Office">
      <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
      <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
      <a:dk2><a:srgbClr val="44546A"/></a:dk2>
      <a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
      <a:accent1><a:srgbClr val="4472C4"/></a:accent1>
      <a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
      <a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>
      <a:accent4><a:srgbClr val="FFC000"/></a:accent4>
      <a:accent5><a:srgbClr val="5B9BD5"/></a:accent5>
      <a:accent6><a:srgbClr val="70AD47"/></a:accent6>
      <a:hlink><a:srgbClr val="0563C1"/></a:hlink>
      <a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
    </a:clrScheme>
  </a:themeElements>
</a:theme>"#;
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="2">
    <numFmt numFmtId="164" formatCode="&quot;¥&quot;#,##0.00"/>
    <numFmt numFmtId="165" formatCode="yyyy/mm/dd"/>
  </numFmts>
  <fonts count="4">
    <font><sz val="11"/><name val="Calibri"/><color theme="1"/></font>
    <font><b/><i/><u val="double"/><strike/><sz val="14.5"/><name val="SimSun"/><color rgb="FF9C0006"/></font>
    <font><b val="700"/><u/><sz val="9"/><name val="Microsoft YaHei"/><color theme="4" tint="0.4"/></font>
    <font><sz val="12"/><color indexed="10"/></font>
  </fonts>
  <fills count="4">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"><fgColor rgb="FF808080"/><bgColor rgb="FFFFFFFF"/></patternFill></fill>
    <fill><patternFill patternType="solid"><fgColor theme="5" tint="-0.25"/><bgColor indexed="64"/></patternFill></fill>
    <fill><patternFill patternType="solid"><fgColor indexed="43"/></patternFill></fill>
  </fills>
  <borders count="2">
    <border><left/><right/><top/><bottom/><diagonal/></border>
    <border diagonalUp="1" diagonalDown="0">
      <left style="thin"><color rgb="FF112233"/></left>
      <right style="mediumDashed"><color theme="4"/></right>
      <top style="double"><color indexed="2"/></top>
      <bottom style="thick"/>
      <diagonal style="thin"><color rgb="FF9C0006"/></diagonal>
    </border>
  </borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="5">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="164" fontId="1" fillId="1" borderId="1" quotePrefix="1">
      <alignment horizontal="center" wrapText="1" indent="2" textRotation="45"/>
      <protection locked="0" hidden="1"/>
    </xf>
    <xf numFmtId="14" fontId="2" fillId="2" borderId="0">
      <alignment horizontal="right" vertical="top" shrinkToFit="1" textRotation="255"/>
    </xf>
    <xf numFmtId="165" fontId="3" fillId="3" borderId="1">
      <alignment vertical="center" textRotation="135"/>
    </xf>
    <xf numFmtId="49" fontId="0" fillId="0" borderId="0">
      <alignment horizontal="distributed" vertical="justify"/>
    </xf>
  </cellXfs>
  <dxfs count="1">
    <dxf><font><b/><color rgb="FF006100"/></font><fill><patternFill><bgColor rgb="FFC6EFCE"/></patternFill></fill></dxf>
  </dxfs>
  <colors>
    <indexedColors>
      <rgbColor rgb="FF000000"/><rgbColor rgb="FFFFFFFF"/><rgbColor rgb="FFFF0000"/><rgbColor rgb="FF00FF00"/>
      <rgbColor rgb="FF0000FF"/><rgbColor rgb="FFFFFF00"/><rgbColor rgb="FFFF00FF"/><rgbColor rgb="FF00FFFF"/>
      <rgbColor rgb="FF000000"/><rgbColor rgb="FFFFFFFF"/><rgbColor rgb="FFAA1122"/>
    </indexedColors>
  </colors>
</styleSheet>"#;

        // Build minimal in-memory XLSX for calamine ground truth
        let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let rels_root = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let wb_xml = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let wb_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        for (name, body) in [
            ("_rels/.rels", rels_root),
            ("xl/workbook.xml", wb_xml),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/theme/theme1.xml", theme_xml),
            ("xl/styles.xml", styles_xml),
            ("xl/worksheets/sheet1.xml", "<worksheet><sheetData/></worksheet>"),
        ] {
            out.start_file(name, opts).unwrap();
            out.write_all(body.as_bytes()).unwrap();
        }
        let mut book = calamine::Xlsx::new(out.finish().unwrap()).unwrap();
        let cal_styles = book.worksheet_cells_reader("S1").unwrap().styles().to_vec();
        let palette = crate::style_theme::parse_palette(theme_xml);
        let (xfs_theme, xfs_res) = crate::style_theme::parse_styles_with_palette(styles_xml, &palette);
        let ext = crate::style_ext::parse(styles_xml);

        let parsed = ParsedStylesBook::parse_xml(styles_xml, Some(theme_xml)).unwrap();
        assert_eq!(parsed.xf_count(), cal_styles.len());
        assert_eq!(parsed.dxfs, crate::cf::parse_dxfs(styles_xml));
        assert_eq!(parsed.dxf_ext, crate::cf::dxfs_unsupported(styles_xml));
        for i in 0..cal_styles.len() {
            let mut expected_cs = super::super::convert_style(&cal_styles[i]);
            crate::style_theme::apply_resolved(
                &mut expected_cs,
                xfs_theme.get(i),
                xfs_res.get(i),
                &palette,
            );
            expected_cs.diagonal = ext
                .xf_border
                .get(i)
                .and_then(|&id| ext.diags.get(id as usize))
                .cloned()
                .flatten();
            let expected_pat = ext
                .xf_fill
                .get(i)
                .and_then(|&id| ext.patterns.get(id as usize))
                .cloned()
                .flatten();
            let expected_quote = ext.xf_quote.get(i).copied().unwrap_or(false);

            let (got_cs, got_theme, got_pat, got_quote) = parsed.resolve_xf(i);
            assert_eq!(got_cs, expected_cs, "xf {i} CellStyle mismatch");
            assert_eq!(got_theme, xfs_theme[i], "xf {i} XfTheme mismatch");
            assert_eq!(got_pat, expected_pat, "xf {i} FillPattern mismatch");
            assert_eq!(got_quote, expected_quote, "xf {i} quotePrefix mismatch");
        }
    }
}
