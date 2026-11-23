use super::SheetApp;
use sheetcore::style::Border;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorderBrush {
    Outline,
    Grid,
    Erase,
}

#[derive(Clone, Copy)]
pub(crate) struct BorderHit {
    pub row: u32,
    pub col: u32,
    pub side: Option<usize>,
}

impl SheetApp {
    pub(crate) fn set_border_brush(&mut self, mode: BorderBrush) {
        self.border_brush = (self.border_brush != Some(mode)).then_some(mode);
        self.border_brush_start = None;
        self.grid_press = None;
        self.format_painter = None;
    }

    pub(super) fn paint_border_drag(
        &mut self,
        start: BorderHit,
        end: BorderHit,
        mode: BorderBrush,
    ) {
        let (r1, c1, r2, c2) = (
            start.row.min(end.row),
            start.col.min(end.col),
            start.row.max(end.row),
            start.col.max(end.col),
        );
        if u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1) > 200_000 {
            self.status = "选区太大（超过 20 万格），请缩小边框范围".into();
            return;
        }
        let side = start.side.filter(|&s| {
            mode != BorderBrush::Grid && ((s < 2 && c1 == c2) || (s >= 2 && r1 == r2))
        });
        let clear = mode == BorderBrush::Erase;
        let pen = if clear {
            Border::default()
        } else {
            Border {
                style: self.border_brush_style,
                color: self.last_border_color,
            }
        };
        let left = c1.checked_sub(1);
        let right = (c2 < 16_383).then_some(c2 + 1);
        let top = r1.checked_sub(1);
        let bottom = (r2 < 1_048_575).then_some(r2 + 1);
        // Both adjoining cells can carry a shared edge. Erase it in one undo transaction.
        let previous = (
            self.sel,
            self.sel_end,
            self.nav_rect.take(),
            std::mem::take(&mut self.extra_sel),
        );
        self.sel = (r1, c1);
        self.sel_end = (r2, c2);
        for (i, strip) in [
            left.map(|c| (r1, c, r2, c)),
            right.map(|c| (r1, c, r2, c)),
            top.map(|r| (r, c1, r, c2)),
            bottom.map(|r| (r, c1, r, c2)),
        ]
        .into_iter()
        .enumerate()
        {
            if side.is_some_and(|selected| selected != i) {
                continue;
            }
            if let Some((top, left, bottom, right)) = strip {
                let sh = self.wb.sheet(self.sheet_idx);
                for r in top..=bottom {
                    for c in left..=right {
                        let opposing = self.wb.styles.get(sh.style_of(r, c)).border[i ^ 1];
                        if opposing.style != 0 && opposing != pen {
                            self.extra_sel.push((r, c, r, c));
                        }
                    }
                }
            }
        }
        self.apply_style_xy(move |st, r, c| {
            if c < c1 {
                st.border[1] = pen;
            } else if c > c2 {
                st.border[0] = pen;
            } else if r < r1 {
                st.border[3] = pen;
            } else if r > r2 {
                st.border[2] = pen;
            } else if let Some(s) = side {
                st.border[s] = pen;
            } else {
                if clear {
                    st.diagonal = None;
                }
                for (i, outside) in [c == c1, c == c2, r == r1, r == r2].into_iter().enumerate() {
                    if outside || mode != BorderBrush::Outline {
                        st.border[i] = pen;
                    }
                }
            }
        });
        (self.sel, self.sel_end, self.nav_rect, self.extra_sel) = previous;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drawn_grid_and_shared_edge_eraser_are_undoable() {
        let _env = crate::test_env::TestEnv::new("draw-borders-undo");
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.border_brush_style = 2;
        app.last_border_color = [100, 20, 35];
        let hit = |row, col, side| BorderHit { row, col, side };
        let borders =
            |app: &SheetApp, r, c| app.wb.styles.get(app.wb.sheet(0).style_of(r, c)).border;
        app.paint_border_drag(hit(2, 3, None), hit(3, 4, None), BorderBrush::Grid);
        let expected = Border {
            style: 2,
            color: [100, 20, 35],
        };
        assert_eq!(borders(&app, 2, 3), [expected; 4]);
        app.paint_border_drag(hit(2, 3, Some(1)), hit(3, 3, Some(1)), BorderBrush::Erase);
        assert_eq!(borders(&app, 2, 3)[1], Border::default());
        assert_eq!(borders(&app, 3, 4)[0], Border::default());
        assert_eq!(borders(&app, 2, 3)[2], expected);
        assert!(app.apply_undo(false));
        assert_eq!(borders(&app, 3, 4), [expected; 4]);
        assert!(app.apply_undo(false));
        assert_eq!(borders(&app, 2, 3), [Border::default(); 4]);

        // Drawing on IV's right edge must not create formatting in column 257 of a BIFF8 form.
        app.paint_border_drag(
            hit(0, 255, Some(1)),
            hit(0, 255, Some(1)),
            BorderBrush::Outline,
        );
        assert_eq!(borders(&app, 0, 255)[1], expected);
        assert_eq!(app.wb.sheet(0).style_of(0, 256), 0);
    }
}
