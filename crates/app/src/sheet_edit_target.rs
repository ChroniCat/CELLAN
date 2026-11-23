use super::SheetApp;

#[derive(Clone)]
pub(super) struct SheetEditTarget {
    doc: u64,
    pub(super) sheet: usize,
    identity: u64,
    coordinates: u64,
    pub(super) room: Option<(u32, String)>,
    pub(super) at: (u32, u32),
}

impl SheetEditTarget {
    pub fn current(app: &SheetApp, at: (u32, u32)) -> Self {
        let sh = app.wb.sheet(app.sheet_idx);
        Self {
            doc: app.doc_uid,
            sheet: app.sheet_idx,
            identity: sh.identity,
            coordinates: sh.coordinate_revision(),
            room: app.net.as_ref().map(|n| (n.site, n.session_id.clone())),
            at,
        }
    }

    pub(super) fn valid(&self, app: &SheetApp) -> bool {
        let Some(sheet) = app.wb.sheets.get(self.sheet) else { return false };
        self.doc == app.doc_uid
            && self.identity == sheet.identity
            && self.coordinates == sheet.coordinate_revision()
            && self.room.as_ref().map(|(site, id)| (*site, id.as_str()))
                == app.net.as_ref().map(|n| (n.site, n.session_id.as_str()))
    }
}
