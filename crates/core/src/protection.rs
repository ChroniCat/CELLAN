//! 工作表保护：SHA-512（Excel 2013+）与旧的 16 位口令哈希。
//!
//! Excel 365 写的是 `<sheetProtection algorithmName="SHA-512" hashValue=".."
//! saltValue=".." spinCount="100000" sheet="1"/>`。哈希算法（实测对齐）：
//!   H0 = SHA512(salt || UTF-16LE(password))
//!   Hi = SHA512(H(i-1) || LE32(i))，i = 0..spinCount-1
//!   hashValue = H(spinCount-1)
//! 旧文件只有 `password="XXXX"`（16 位哈希的十六进制）。

use base64::Engine;
use sha2::{Digest, Sha512};

#[derive(Clone, Debug, PartialEq)]
pub struct SheetProtection {
    /// "SHA-512" 或空（旧格式）
    pub algorithm: String,
    /// base64 的 hashValue（SHA-512 模式）
    pub hash: String,
    /// base64 的 saltValue
    pub salt: String,
    pub spin: u32,
    /// 旧格式的 16 位哈希
    pub legacy: Option<u16>,
    pub sheet: bool,
    pub objects: bool,
    pub scenarios: bool,
    /// 受保护时允许的操作（Excel「保护工作表」对话框里那组勾选）
    pub perms: ProtPerms,
}

/// 受保护工作表上允许哪些操作，对应 `sheetProtection` 的布尔属性（true = 允许）。
///
/// OOXML 里这些属性缺省是 **0 = 禁止**，Excel 也只写「允许」的那几个；
/// 所以解析时缺省一律 false，写回时显式写 0/1，避免重开后被别的软件按缺省值放宽。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtPerms {
    pub select_locked: bool,
    pub select_unlocked: bool,
    pub format_cells: bool,
    pub format_columns: bool,
    pub format_rows: bool,
    pub insert_columns: bool,
    pub insert_rows: bool,
    pub delete_columns: bool,
    pub delete_rows: bool,
    pub insert_links: bool,
    pub sort: bool,
    pub auto_filter: bool,
    pub pivot_tables: bool,
    pub edit_objects: bool,
    pub edit_scenarios: bool,
}

impl Default for ProtPerms {
    fn default() -> Self {
        Self {
            select_locked: false,
            select_unlocked: false,
            format_cells: false,
            format_columns: false,
            format_rows: false,
            insert_columns: false,
            insert_rows: false,
            delete_columns: false,
            delete_rows: false,
            insert_links: false,
            sort: false,
            auto_filter: false,
            pivot_tables: false,
            edit_objects: false,
            edit_scenarios: false,
        }
    }
}

impl ProtPerms {
    /// Excel「保护工作表」默认勾选：允许选择锁定/未锁定单元格，其余全禁。
    pub const fn select_only() -> Self {
        Self {
            select_locked: true,
            select_unlocked: true,
            format_cells: false,
            format_columns: false,
            format_rows: false,
            insert_columns: false,
            insert_rows: false,
            delete_columns: false,
            delete_rows: false,
            insert_links: false,
            sort: false,
            auto_filter: false,
            pivot_tables: false,
            edit_objects: false,
            edit_scenarios: false,
        }
    }

    pub fn get(&self, op: ProtOp) -> bool {
        match op {
            ProtOp::SelectLockedCells => self.select_locked,
            ProtOp::SelectUnlockedCells => self.select_unlocked,
            ProtOp::FormatCells => self.format_cells,
            ProtOp::FormatColumns => self.format_columns,
            ProtOp::FormatRows => self.format_rows,
            ProtOp::InsertColumns => self.insert_columns,
            ProtOp::InsertRows => self.insert_rows,
            ProtOp::DeleteColumns => self.delete_columns,
            ProtOp::DeleteRows => self.delete_rows,
            ProtOp::InsertHyperlinks => self.insert_links,
            ProtOp::Sort => self.sort,
            ProtOp::AutoFilter => self.auto_filter,
            ProtOp::PivotTables => self.pivot_tables,
            ProtOp::EditObjects => self.edit_objects,
            ProtOp::EditScenarios => self.edit_scenarios,
        }
    }

    /// Excel 实测写法（`AllowFormattingCells:=True` → `<sheetProtection sheet="1" formatCells="0"/>`）：
    /// 这些属性的值是「保护开关」——`0` 表示允许，整条缺失表示禁止；默认值就是禁止，
    /// 所以只写允许的那几条，和 Excel 保存出来的 XML 一字不差。
    fn to_attrs(&self) -> String {
        let mut s = String::new();
        let allows = [
            ("formatCells", self.format_cells),
            ("formatColumns", self.format_columns),
            ("formatRows", self.format_rows),
            ("insertColumns", self.insert_columns),
            ("insertRows", self.insert_rows),
            ("deleteColumns", self.delete_columns),
            ("deleteRows", self.delete_rows),
            ("insertHyperlinks", self.insert_links),
            ("sort", self.sort),
            ("autoFilter", self.auto_filter),
            ("pivotTables", self.pivot_tables),
        ];
        for (name, on) in allows {
            if on {
                s.push_str(&format!(" {name}=\"0\""));
            }
        }
        // 选择单元格默认是允许的，所以「禁止选择」才需要显式写 1
        if !self.select_locked {
            s.push_str(" selectLockedCells=\"1\"");
        }
        if !self.select_unlocked {
            s.push_str(" selectUnlockedCells=\"1\"");
        }
        s
    }
}

/// 受保护表上的一次操作（用来查权限，不参与序列化）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtOp {
    SelectLockedCells,
    SelectUnlockedCells,
    FormatCells,
    FormatColumns,
    FormatRows,
    InsertColumns,
    InsertRows,
    DeleteColumns,
    DeleteRows,
    InsertHyperlinks,
    Sort,
    AutoFilter,
    PivotTables,
    EditObjects,
    EditScenarios,
}

impl Default for SheetProtection {
    /// 「存在保护」的默认值：打开保护、按 Excel 对话框默认只允许选择单元格。
    /// `sheet` 显式给 true——模型里 `Some(protection)` 就代表这张表是受保护的，
    /// 写成 sheet="0" 会让别的软件和本项目的解析都当成未保护。
    fn default() -> Self {
        Self {
            algorithm: String::new(),
            hash: String::new(),
            salt: String::new(),
            spin: 0,
            legacy: None,
            sheet: true,
            objects: true,
            scenarios: true,
            perms: ProtPerms::select_only(),
        }
    }
}

impl SheetProtection {
    /// 用密码生成一份新的保护（SHA-512，随机盐）
    pub fn new_with_password(pw: &str) -> Self {
        let salt = random_salt();
        let spin = 100_000u32;
        let hash = hash_password(pw, &salt, spin);
        Self {
            algorithm: "SHA-512".into(),
            hash: base64::engine::general_purpose::STANDARD.encode(hash),
            salt: base64::engine::general_purpose::STANDARD.encode(salt),
            spin,
            legacy: None,
            sheet: true,
            objects: true,
            scenarios: true,
            perms: ProtPerms::select_only(),
        }
    }

    /// 这项操作是否被允许（非受保护表由调用方直接放行，不走这里）。
    pub fn can(&self, op: ProtOp) -> bool {
        self.perms.get(op)
    }

    pub fn verify(&self, pw: &str) -> bool {
        if self.algorithm.eq_ignore_ascii_case("SHA-512") && !self.salt.is_empty() {
            let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(&self.salt) else {
                return false;
            };
            let Ok(want) = base64::engine::general_purpose::STANDARD.decode(&self.hash) else {
                return false;
            };
            let got = hash_password(pw, &salt, self.spin.max(1));
            return got == want;
        }
        match self.legacy {
            Some(h) => legacy_hash(pw) == h,
            None => true,
        }
    }

    pub fn to_xml(&self) -> String {
        let mut attrs = String::new();
        if self.algorithm.eq_ignore_ascii_case("SHA-512") && !self.hash.is_empty() {
            attrs.push_str(&format!(
                " algorithmName=\"SHA-512\" hashValue=\"{}\" saltValue=\"{}\" spinCount=\"{}\"",
                self.hash, self.salt, self.spin.max(1)
            ));
        } else if let Some(h) = self.legacy {
            attrs.push_str(&format!(" password=\"{:04X}\"", h));
        }
        attrs.push_str(&format!(
            " sheet=\"{}\" objects=\"{}\" scenarios=\"{}\"",
            self.sheet as u8, self.objects as u8, self.scenarios as u8
        ));
        attrs.push_str(&self.perms.to_attrs());
        format!("<sheetProtection{attrs}/>")
    }
}

pub fn hash_password(pw: &str, salt: &[u8], spin: u32) -> Vec<u8> {
    let mut h = Sha512::new();
    h.update(salt);
    for unit in pw.encode_utf16() {
        h.update(unit.to_le_bytes());
    }
    let mut cur = h.finalize().to_vec();
    for i in 0..spin {
        let mut d = Sha512::new();
        d.update(&cur);
        d.update(i.to_le_bytes());
        cur = d.finalize().to_vec();
    }
    cur
}

/// 旧格式（BIFF8 / 早期 xlsx）的 16 位哈希
pub fn legacy_hash(pw: &str) -> u16 {
    // Excel/BIFF8 口径（MS-OFFCRYPTO 的 worksheet verifier）：
    // 从最后一个字符往前，每步先循环左移一位再异或字符；最后再移一位，
    // 异或长度与 0xCE4B。s3cret → 0xDBFF、password → 0x83AF（Excel 文件实测）。
    let mut hash: u16 = 0;
    for ch in pw.chars().rev() {
        hash = ((hash >> 14) & 0x01) | ((hash << 1) & 0x7FFF);
        hash ^= ch as u16;
    }
    hash = ((hash >> 14) & 0x01) | ((hash << 1) & 0x7FFF);
    hash ^= pw.chars().count() as u16;
    hash ^= 0xCE4B;
    hash
}

fn random_salt() -> Vec<u8> {
    // 盐只要求唯一，不要求保密：时间 + 进程 + 地址搅一遍再 SHA-512
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let stack = &nanos as *const _ as usize;
    let mut d = Sha512::new();
    d.update(nanos.to_le_bytes());
    d.update(pid.to_le_bytes());
    d.update(stack.to_le_bytes());
    d.finalize()[..16].to_vec()
}

/// 从 `<sheetProtection .../>` 标签里解析
pub fn parse_tag(tag: &str) -> Option<SheetProtection> {
    let attr = |name: &str| -> Option<String> {
        let pat = format!("{name}=\"");
        let i = tag.find(&pat)? + pat.len();
        let rest = &tag[i..];
        let j = rest.find('"')?;
        Some(rest[..j].to_string())
    };
    let algorithm = attr("algorithmName").unwrap_or_default();
    let hash = attr("hashValue").unwrap_or_default();
    let salt = attr("saltValue").unwrap_or_default();
    let spin = attr("spinCount").and_then(|v| v.parse().ok()).unwrap_or(0);
    let legacy = attr("password").and_then(|v| u16::from_str_radix(v.trim(), 16).ok());
    let sheet = attr("sheet").map(|v| v == "1" || v == "true").unwrap_or(true);
    if !sheet && hash.is_empty() && legacy.is_none() {
        return None;
    }
    // 这些属性是「保护开关」：0/false = 允许，1/true = 禁止（Excel 只写允许的那几条）。
    let allowed = |name: &str| -> bool {
        matches!(attr(name).as_deref(), Some("0") | Some("false"))
    };
    let objects = attr("objects").map(|v| v == "1").unwrap_or(true);
    let scenarios = attr("scenarios").map(|v| v == "1").unwrap_or(true);
    let perms = ProtPerms {
        // 选择默认允许：只有显式 1 才是禁止
        select_locked: !matches!(attr("selectLockedCells").as_deref(), Some("1") | Some("true")),
        select_unlocked: !matches!(attr("selectUnlockedCells").as_deref(), Some("1") | Some("true")),
        format_cells: allowed("formatCells"),
        format_columns: allowed("formatColumns"),
        format_rows: allowed("formatRows"),
        insert_columns: allowed("insertColumns"),
        insert_rows: allowed("insertRows"),
        delete_columns: allowed("deleteColumns"),
        delete_rows: allowed("deleteRows"),
        insert_links: allowed("insertHyperlinks"),
        sort: allowed("sort"),
        auto_filter: allowed("autoFilter"),
        pivot_tables: allowed("pivotTables"),
        // objects=1 表示对象受保护 → 不能编辑对象；反之可以
        edit_objects: if attr("editObjects").is_some() { allowed("editObjects") } else { !objects },
        edit_scenarios: if attr("editScenarios").is_some() { allowed("editScenarios") } else { !scenarios },
    };
    Some(SheetProtection {
        algorithm,
        hash,
        salt,
        spin,
        legacy,
        sheet,
        objects,
        scenarios,
        perms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha512_matches_excel_reference() {
        // 由 Excel 365 生成：密码 "test"，spinCount 100000
        let salt = base64::engine::general_purpose::STANDARD
            .decode("iSCmYeo9C/KCvdr/wWS9VA==")
            .unwrap();
        let want = base64::engine::general_purpose::STANDARD
            .decode("a8QwQr9Et7Bm+zSa+DePDv+zB8WnNaBU3WjBhu7km7s80y42RP7cR7bWsHWoh+wAz7pfvpzjym3mjpzHOBX6bg==")
            .unwrap();
        assert_eq!(hash_password("test", &salt, 100000), want);
    }

    /// C15：15 个权限位必须原样往返（Excel 只写「允许」的属性，缺省=禁止）
    #[test]
    fn permission_flags_round_trip() {
        // Excel 实测：允许的项写 "0"，禁止的项整条不写
        let tag = "<sheetProtection algorithmName=\"SHA-512\" hashValue=\"h\" saltValue=\"s\" spinCount=\"100000\" sheet=\"1\" objects=\"0\" scenarios=\"1\" selectLockedCells=\"1\" formatCells=\"0\" insertRows=\"0\" deleteRows=\"0\" sort=\"0\" autoFilter=\"0\"/>";
        let p = parse_tag(tag).expect("应解析出保护");
        assert!(!p.perms.select_locked, "selectLockedCells=1 表示禁止选择锁定格");
        assert!(p.perms.select_unlocked, "缺省允许选择未锁定格");
        assert!(p.perms.format_cells);
        assert!(!p.perms.format_columns, "缺省不允许设置列格式");
        assert!(p.perms.insert_rows && p.perms.delete_rows && p.perms.sort && p.perms.auto_filter);
        assert!(!p.perms.insert_columns);
        assert!(p.perms.edit_objects, "objects=0 → 可编辑对象");
        assert!(!p.perms.edit_scenarios, "scenarios=1 → 方案受保护");

        let again = parse_tag(&p.to_xml()).expect("写回后应能再解析");
        assert_eq!(again.perms, p.perms, "权限位往返不能丢");
        assert_eq!(again.objects, p.objects);
        assert_eq!(again.scenarios, p.scenarios);
        assert_eq!(again.hash, p.hash);
    }

    /// 保护对话框默认值：只允许选择单元格
    #[test]
    fn default_protection_allows_only_selection() {
        let p = SheetProtection::new_with_password("");
        assert!(p.can(ProtOp::SelectLockedCells));
        assert!(p.can(ProtOp::SelectUnlockedCells));
        assert!(!p.can(ProtOp::FormatCells));
        assert!(!p.can(ProtOp::Sort));
        assert!(!p.can(ProtOp::InsertRows));
    }
    #[test]
    fn legacy_hash_matches_excel_vector() {
        // Excel 生成的受保护 .xls 里 PASSWORD 记录就是 0xFFDB。
        assert_eq!(legacy_hash("s3cret"), 0xDBFF);
        assert_eq!(legacy_hash(""), 0xCE4B);
    }

    #[test]
    fn protect_and_verify() {
        let p = SheetProtection::new_with_password("s3cret");
        assert!(p.verify("s3cret"));
        assert!(!p.verify("wrong"));
        let xml = p.to_xml();
        let parsed = parse_tag(&xml).expect("应能解析");
        assert!(parsed.verify("s3cret"));
        assert_eq!(parsed.algorithm, "SHA-512");
    }

    #[test]
    fn legacy_hash_is_stable() {
        // 同一口令两次结果一致，且长度异或参与
        assert_eq!(legacy_hash("abc"), legacy_hash("abc"));
        assert_ne!(legacy_hash("abc"), legacy_hash("abd"));
    }
}
