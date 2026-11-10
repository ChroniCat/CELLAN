//! 界面图标：24×24 网格上自己画，不依赖任何图标字体或第三方素材。
//!
//! 这样做有两个原因：一是随包可分发的授权要干净（只用自己画的线条）；二是
//! egui 默认字体里没有这些符号，用文字硬凑在不同字体下会歪。

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, Stroke};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Open,
    Save,
    SaveAs,
    Print,
    Undo,
    Redo,
    Paste,
    Cut,
    Copy,
    Brush,
    Fill,
    FontColor,
    AlignL,
    AlignC,
    AlignR,
    Top,
    Middle,
    Bottom,
    Wrap,
    Merge,
    Border,
    Currency,
    Percent,
    Comma,
    SortAsc,
    SortDesc,
    Filter,
    Find,
    Chart,
    Image,
    Table,
    Comment,
    Pivot,
    Link,
    Revision,
    Zoom,
    Calc,
    Sheet,
    Share,
    Fx,
    Sum,
    Grid,
    Close,
    Plus,
    Refresh,
    DecInc,
    DecDec,
    FontGrow,
    FontShrink,
    Freeze,
    Lock,
    /// 条件格式：三条彩色数据条（Excel 的那个图标）
    Cf,
}

struct Pen<'a> {
    p: &'a Painter,
    r: Rect,
    c: Color32,
    w: f32,
}

impl Pen<'_> {
    fn xy(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.r.left() + x * self.r.width() / 24.0,
            self.r.top() + y * self.r.height() / 24.0,
        )
    }

    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.p
            .line_segment([self.xy(a.0, a.1), self.xy(b.0, b.1)], Stroke::new(self.w, self.c));
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) {
        self.p.rect_stroke(
            Rect::from_min_max(self.xy(x, y), self.xy(x + w, y + h)),
            1.5,
            Stroke::new(self.w, self.c),
            egui::StrokeKind::Inside,
        );
    }

    fn frect(&self, x: f32, y: f32, w: f32, h: f32) {
        self.p.rect_filled(
            Rect::from_min_max(self.xy(x, y), self.xy(x + w, y + h)),
            1.0,
            self.c,
        );
    }

    fn circle(&self, x: f32, y: f32, rad: f32) {
        self.p
            .circle_stroke(self.xy(x, y), rad * self.r.width() / 24.0, Stroke::new(self.w, self.c));
    }

    fn fcircle(&self, x: f32, y: f32, rad: f32) {
        self.p
            .circle_filled(self.xy(x, y), rad * self.r.width() / 24.0, self.c);
    }

    fn poly(&self, pts: &[(f32, f32)]) {
        let v: Vec<Pos2> = pts.iter().map(|q| self.xy(q.0, q.1)).collect();
        self.p
            .add(egui::Shape::convex_polygon(v, self.c, Stroke::NONE));
    }

    fn polyline(&self, pts: &[(f32, f32)]) {
        let v: Vec<Pos2> = pts.iter().map(|q| self.xy(q.0, q.1)).collect();
        self.p.add(egui::Shape::line(v, Stroke::new(self.w, self.c)));
    }

    /// 箭头（沿 dx/dy 方向的小三角）
    fn arrow_head(&self, tip: (f32, f32), dir: (f32, f32), size: f32) {
        let (dx, dy) = dir;
        let n = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / n, dy / n);
        let (px, py) = (-uy, ux);
        self.poly(&[
            (tip.0, tip.1),
            (tip.0 - ux * size + px * size * 0.55, tip.1 - uy * size + py * size * 0.55),
            (tip.0 - ux * size - px * size * 0.55, tip.1 - uy * size - py * size * 0.55),
        ]);
    }

    fn text(&self, s: &str, size: f32) {
        self.p.text(
            self.xy(12.0, 13.0),
            Align2::CENTER_CENTER,
            s,
            FontId::proportional(size * self.r.width() / 24.0),
            self.c,
        );
    }
}

pub fn draw(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let pen = Pen {
        p,
        r: rect,
        c: color,
        w: 1.8 * rect.width() / 24.0,
    };
    draw_with_pen(pen, icon);
}

/// Office 色板：图标底色与描边都用这套，视觉上和 Excel 的彩色图标同一色系。
const BLUE: Color32 = Color32::from_rgb(68, 114, 196);
const BLUE_D: Color32 = Color32::from_rgb(31, 78, 121);
const BLUE_L: Color32 = Color32::from_rgb(189, 215, 238);
const BROWN: Color32 = Color32::from_rgb(166, 88, 42);
const BROWN_D: Color32 = Color32::from_rgb(132, 60, 12);
const CREAM: Color32 = Color32::from_rgb(255, 242, 204);
const GOLD: Color32 = Color32::from_rgb(255, 192, 0);
const GOLD_D: Color32 = Color32::from_rgb(127, 96, 0);
const RED: Color32 = Color32::from_rgb(192, 0, 0);
const GREEN: Color32 = Color32::from_rgb(112, 173, 71);
const GREEN_D: Color32 = Color32::from_rgb(84, 130, 53);
const ORANGE: Color32 = Color32::from_rgb(237, 125, 49);
const ORANGE_D: Color32 = Color32::from_rgb(197, 90, 17);
const GRAY_D: Color32 = Color32::from_rgb(89, 89, 89);

/// 图标的线条色：Excel 的图标不是纯黑线条，而是各自主题色的深色版。
fn stroke_of(icon: Icon) -> Option<Color32> {
    use Icon::*;
    Some(match icon {
        Paste => BROWN_D,
        Cut | Copy | Brush => BLUE_D,
        Fill => GOLD_D,
        FontColor => RED,
        Border | Grid => GRAY_D,
        AlignL | AlignC | AlignR | Top | Middle | Bottom | Wrap | Merge => BLUE_D,
        Currency | Percent | Comma | DecInc | DecDec => GREEN_D,
        SortAsc | SortDesc | Filter | Link | Table | Pivot | Share => BLUE_D,
        Find | Zoom | Revision | Calc => GRAY_D,
        Chart => ORANGE_D,
        Image => GREEN_D,
        Comment => GOLD_D,
        Cf => GRAY_D,
        Sheet => GREEN_D,
        Sum | Fx => GRAY_D,
        _ => return None,
    })
}

/// Excel 风格：先铺一层彩色底（和 Excel 自带的彩色图标同色系），再按主题色描边。
/// 关掉的按钮只画灰线，不铺色。
pub fn draw_excel(p: &Painter, rect: Rect, icon: Icon, color: Color32, enabled: bool) {
    // Excel 的图形比 24 网格画得满：外扩 4%，32px 时约多 2.5px
    let rect = rect.expand(rect.width() * 0.04);
    let s = rect.width() / 24.0;
    let at = |x: f32, y: f32| egui::pos2(rect.left() + x * s, rect.top() + y * s);
    let fill = |x: f32, y: f32, w: f32, h: f32, c: Color32| {
        p.rect_filled(
            Rect::from_min_size(at(x, y), egui::vec2(w * s, h * s)),
            0.8 * s,
            c,
        );
    };
    if enabled {
        use Icon::*;
        match icon {
            // 粘贴：棕色剪贴板 + 米色纸 + 蓝夹子
            Paste => {
                fill(5.0, 3.5, 14.0, 17.5, BROWN);
                fill(7.0, 6.5, 10.0, 12.5, CREAM);
                fill(9.5, 2.5, 5.0, 3.2, BLUE);
            }
            Cut => {
                // 剪刀的两片刃用蓝色，环用深蓝
                p.line_segment(
                    [at(7.0, 4.0), at(16.0, 15.0)],
                    Stroke::new(2.2 * s, BLUE),
                );
                p.line_segment(
                    [at(17.0, 4.0), at(8.0, 15.0)],
                    Stroke::new(2.2 * s, BLUE),
                );
                p.circle_stroke(at(6.0, 18.0), 2.4 * s, Stroke::new(1.8 * s, BLUE_D));
                p.circle_stroke(at(18.0, 18.0), 2.4 * s, Stroke::new(1.8 * s, BLUE_D));
            }
            Copy => {
                fill(8.4, 3.4, 11.6, 13.6, BLUE_L);
                fill(4.4, 7.4, 11.6, 13.6, CREAM);
            }
            Brush => {
                fill(4.2, 4.2, 8.6, 11.6, Color32::from_rgb(226, 196, 138));
                fill(1.0, 17.6, 3.2, 3.2, BLUE_D);
            }
            Fill => {
                // 油漆桶：桶身金黄、流出的那一滴也金黄
                p.add(egui::Shape::convex_polygon(
                    vec![at(6.0, 11.0), at(12.0, 4.0), at(18.0, 11.0), at(12.0, 18.0)],
                    GOLD,
                    Stroke::new(1.2 * s, GOLD_D),
                ));
                p.add(egui::Shape::convex_polygon(
                    vec![at(19.0, 14.5), at(21.5, 20.5), at(16.5, 20.5)],
                    GOLD,
                    Stroke::new(1.0 * s, GOLD_D),
                ));
            }
            FontColor => {
                fill(5.0, 19.6, 14.0, 3.2, RED);
            }
            Border => {
                // 外框用蓝色，内部两条线浅蓝
                p.rect_stroke(
                    Rect::from_min_max(at(4.0, 4.0), at(20.0, 20.0)),
                    0.0,
                    Stroke::new(2.0 * s, BLUE),
                    egui::StrokeKind::Inside,
                );
                fill(4.0, 11.2, 16.0, 1.6, BLUE_L);
                fill(11.2, 4.0, 1.6, 16.0, BLUE_L);
            }
            Chart => {
                fill(4.6, 12.4, 4.2, 7.4, BLUE);
                fill(9.8, 7.4, 4.2, 12.4, ORANGE);
                fill(15.0, 10.4, 4.2, 9.4, Color32::from_rgb(165, 165, 165));
            }
            Image => {
                fill(3.6, 5.6, 16.8, 12.8, BLUE_L);
                fill(3.6, 13.4, 7.4, 5.0, GREEN);
                fill(9.6, 11.6, 10.8, 6.8, GREEN_D);
                p.circle_filled(at(8.0, 9.0), 1.7 * s, GOLD);
            }
            Table => {
                fill(3.6, 4.8, 16.8, 4.6, BLUE);
                fill(3.6, 9.4, 16.8, 9.2, BLUE_L);
            }
            Comment => {
                fill(4.6, 4.6, 14.8, 10.0, CREAM);
            }
            Link => {
                p.circle_stroke(at(8.0, 12.0), 4.0 * s, Stroke::new(2.0 * s, BLUE));
                p.circle_stroke(at(16.0, 12.0), 4.0 * s, Stroke::new(2.0 * s, BLUE));
            }
            Revision | Zoom | Find => {
                p.circle_filled(at(11.0, 10.0), 5.6 * s, BLUE_L);
            }
            Calc => {
                fill(4.6, 3.6, 14.8, 16.8, Color32::from_rgb(231, 230, 230));
            }
            Sheet => {
                fill(4.6, 3.6, 14.8, 16.8, Color32::from_rgb(226, 239, 218));
            }
            Filter => {
                fill(5.4, 5.4, 13.2, 4.4, BLUE);
            }
            SortAsc | SortDesc => {
                fill(14.6, 4.6, 4.8, 12.8, BLUE);
            }
            Share => {
                p.circle_filled(at(17.0, 6.0), 2.4 * s, BLUE);
                p.circle_filled(at(6.0, 12.0), 2.4 * s, BLUE);
                p.circle_filled(at(17.0, 18.0), 2.4 * s, BLUE);
            }
            Save | SaveAs => {
                fill(4.6, 4.6, 14.8, 14.8, BLUE_L);
            }
            Open => {
                fill(3.8, 8.0, 14.4, 9.2, GOLD);
            }
            Print => {
                fill(3.8, 9.8, 16.4, 6.2, BLUE_L);
            }
            Sum => {
                // Σ 用绿色，Excel 的自动求和图标是绿色
                fill(6.0, 16.4, 12.0, 2.4, GREEN);
            }
            Cf => {
                // 条件格式：三条长度递减的彩色数据条
                fill(4.4, 5.0, 14.0, 2.6, BLUE);
                fill(4.4, 10.4, 9.6, 2.6, ORANGE);
                fill(4.4, 15.8, 6.0, 2.6, GREEN);
            }
            _ => {}
        }
    }
    // 线条色：彩色图标用主题深色，其余跟随文字色；关掉的按钮一律灰
    let line_col = if enabled {
        stroke_of(icon).unwrap_or(color)
    } else {
        color
    };
    // 32px 的大图标线宽 1.8 会显得又黑又粗，按尺寸收到 1.5；16px 时保底 1px
    let w = (1.5 * s).max(1.0);
    let pen = Pen {
        p,
        r: rect,
        c: line_col,
        w,
    };
    draw_with_pen(pen, icon);
}

fn draw_with_pen(pen: Pen<'_>, icon: Icon) {
    use Icon::*;
    match icon {
        Open => {
            pen.polyline(&[(3.0, 19.0), (3.0, 6.0), (10.0, 6.0), (12.0, 9.0), (21.0, 9.0), (21.0, 19.0), (3.0, 19.0)]);
            pen.line((3.0, 12.0), (21.0, 12.0));
        }
        Save => {
            pen.rect(4.0, 4.0, 16.0, 16.0);
            pen.frect(8.0, 4.0, 8.0, 5.0);
            pen.rect(7.0, 13.0, 10.0, 7.0);
        }
        SaveAs => {
            pen.rect(3.0, 4.0, 14.0, 16.0);
            pen.frect(6.0, 4.0, 7.0, 4.0);
            pen.line((16.0, 20.0), (16.0, 13.0));
            pen.arrow_head((16.0, 12.0), (0.0, -1.0), 4.0);
        }
        Print => {
            pen.rect(6.0, 3.0, 12.0, 6.0);
            pen.rect(3.0, 9.0, 18.0, 8.0);
            pen.rect(6.0, 14.0, 12.0, 7.0);
            pen.line((7.0, 17.0), (17.0, 17.0));
        }
        Undo => {
            pen.polyline(&[(8.0, 7.0), (8.0, 16.0), (17.0, 16.0)]);
            pen.line((8.0, 7.0), (12.0, 7.0));
            pen.arrow_head((6.0, 7.0), (-1.0, 0.0), 4.0);
        }
        Redo => {
            pen.polyline(&[(16.0, 7.0), (16.0, 16.0), (7.0, 16.0)]);
            pen.line((16.0, 7.0), (12.0, 7.0));
            pen.arrow_head((18.0, 7.0), (1.0, 0.0), 4.0);
        }
        Paste => {
            pen.rect(5.0, 4.0, 14.0, 17.0);
            pen.frect(9.0, 2.5, 6.0, 4.0);
            pen.line((8.0, 11.0), (16.0, 11.0));
            pen.line((8.0, 15.0), (16.0, 15.0));
        }
        Cut => {
            pen.line((7.0, 4.0), (16.0, 16.0));
            pen.line((17.0, 4.0), (8.0, 16.0));
            pen.circle(6.0, 18.0, 2.4);
            pen.circle(18.0, 18.0, 2.4);
        }
        Copy => {
            pen.rect(8.0, 3.0, 12.0, 14.0);
            pen.rect(4.0, 7.0, 12.0, 14.0);
        }
        Brush => {
            pen.rect(4.0, 4.0, 9.0, 12.0);
            pen.line((13.0, 7.0), (20.0, 7.0));
            pen.line((20.0, 7.0), (20.0, 18.0));
            pen.line((17.0, 18.0), (23.0, 18.0));
            pen.frect(1.0, 18.0, 3.0, 3.0);
        }
        Fill => {
            pen.polyline(&[(6.0, 11.0), (12.0, 4.0), (18.0, 11.0), (12.0, 18.0), (6.0, 11.0)]);
            pen.line((12.0, 4.0), (12.0, 1.5));
            pen.line((19.0, 15.0), (21.0, 20.0));
            pen.line((21.0, 20.0), (17.0, 20.0));
            pen.line((17.0, 20.0), (19.0, 15.0));
        }
        FontColor => {
            pen.text("A", 15.0);
            pen.frect(5.0, 20.0, 14.0, 2.6);
        }
        AlignL => {
            for (i, w) in [16.0, 10.0, 14.0, 8.0].iter().enumerate() {
                pen.line((4.0, 5.0 + i as f32 * 4.5), (4.0 + w, 5.0 + i as f32 * 4.5));
            }
        }
        AlignC => {
            for (i, w) in [16.0, 10.0, 14.0, 8.0].iter().enumerate() {
                let x = 12.0 - w / 2.0;
                pen.line((x, 5.0 + i as f32 * 4.5), (x + w, 5.0 + i as f32 * 4.5));
            }
        }
        AlignR => {
            for (i, w) in [16.0, 10.0, 14.0, 8.0].iter().enumerate() {
                pen.line((20.0 - w, 5.0 + i as f32 * 4.5), (20.0, 5.0 + i as f32 * 4.5));
            }
        }
        Top => {
            pen.line((4.0, 4.0), (20.0, 4.0));
            pen.frect(9.0, 7.0, 6.0, 4.0);
            pen.rect(4.0, 13.0, 16.0, 7.0);
        }
        Middle => {
            pen.line((4.0, 4.0), (20.0, 4.0));
            pen.line((4.0, 20.0), (20.0, 20.0));
            pen.frect(9.0, 10.0, 6.0, 4.0);
        }
        Bottom => {
            pen.frect(9.0, 4.0, 6.0, 4.0);
            pen.rect(4.0, 4.0, 16.0, 7.0);
            pen.line((4.0, 20.0), (20.0, 20.0));
        }
        Wrap => {
            pen.line((4.0, 6.0), (20.0, 6.0));
            pen.line((4.0, 12.0), (14.0, 12.0));
            pen.line((4.0, 18.0), (20.0, 18.0));
            pen.polyline(&[(15.0, 9.0), (18.0, 12.0), (15.0, 15.0)]);
        }
        Merge => {
            pen.rect(3.0, 6.0, 7.0, 12.0);
            pen.rect(14.0, 6.0, 7.0, 12.0);
            pen.line((10.0, 12.0), (14.0, 12.0));
            pen.arrow_head((15.0, 12.0), (1.0, 0.0), 3.0);
            pen.arrow_head((9.0, 12.0), (-1.0, 0.0), 3.0);
        }
        Border => {
            pen.rect(4.0, 4.0, 16.0, 16.0);
            pen.line((12.0, 4.0), (12.0, 20.0));
            pen.line((4.0, 12.0), (20.0, 12.0));
        }
        Currency => {
            pen.text("¥", 15.0);
        }
        Percent => {
            pen.text("%", 14.0);
        }
        Comma => {
            pen.text(",", 16.0);
        }
        SortAsc => {
            pen.line((4.0, 7.0), (11.0, 7.0));
            pen.line((4.0, 12.0), (9.0, 12.0));
            pen.line((4.0, 17.0), (7.0, 17.0));
            pen.line((17.0, 19.0), (17.0, 5.0));
            pen.arrow_head((17.0, 4.0), (0.0, -1.0), 4.0);
        }
        SortDesc => {
            pen.line((4.0, 7.0), (7.0, 7.0));
            pen.line((4.0, 12.0), (9.0, 12.0));
            pen.line((4.0, 17.0), (11.0, 17.0));
            pen.line((17.0, 5.0), (17.0, 19.0));
            pen.arrow_head((17.0, 20.0), (0.0, 1.0), 4.0);
        }
        Filter => {
            pen.polyline(&[(4.0, 5.0), (20.0, 5.0), (14.0, 12.0), (14.0, 19.0), (10.0, 17.0), (10.0, 12.0), (4.0, 5.0)]);
        }
        Find => {
            pen.circle(11.0, 10.0, 6.0);
            pen.line((15.5, 14.5), (21.0, 20.0));
        }
        Chart => {
            pen.line((3.0, 20.0), (21.0, 20.0));
            pen.frect(5.0, 12.0, 4.0, 8.0);
            pen.frect(10.0, 7.0, 4.0, 13.0);
            pen.frect(15.0, 10.0, 4.0, 10.0);
        }
        Image => {
            pen.rect(3.0, 5.0, 18.0, 14.0);
            pen.circle(8.0, 9.5, 1.6);
            pen.polyline(&[(3.0, 17.0), (10.0, 11.0), (15.0, 16.0), (18.0, 13.0), (21.0, 16.0)]);
        }
        Table => {
            pen.rect(3.0, 5.0, 18.0, 14.0);
            pen.line((3.0, 9.5), (21.0, 9.5));
            pen.line((3.0, 14.5), (21.0, 14.5));
            pen.line((9.0, 5.0), (9.0, 19.0));
            pen.line((15.0, 5.0), (15.0, 19.0));
        }
        Comment => {
            pen.polyline(&[(4.0, 4.0), (20.0, 4.0), (20.0, 15.0), (10.0, 15.0), (6.0, 19.0), (6.0, 15.0), (4.0, 15.0), (4.0, 4.0)]);
        }
        Pivot => {
            pen.rect(3.0, 4.0, 12.0, 10.0);
            pen.line((3.0, 8.0), (15.0, 8.0));
            pen.line((8.0, 4.0), (8.0, 14.0));
            pen.line((14.0, 19.0), (21.0, 19.0));
            pen.arrow_head((22.0, 19.0), (1.0, 0.0), 3.0);
            pen.line((17.0, 15.0), (17.0, 19.0));
        }
        Link => {
            pen.circle(8.0, 12.0, 4.0);
            pen.circle(16.0, 12.0, 4.0);
            pen.line((11.0, 12.0), (13.0, 12.0));
        }
        Revision => {
            pen.circle(12.0, 12.0, 8.0);
            pen.line((12.0, 12.0), (12.0, 7.0));
            pen.line((12.0, 12.0), (16.0, 14.0));
        }
        Zoom => {
            pen.circle(11.0, 10.0, 6.0);
            pen.line((15.5, 14.5), (21.0, 20.0));
            pen.line((8.0, 10.0), (14.0, 10.0));
            pen.line((11.0, 7.0), (11.0, 13.0));
        }
        Calc => {
            pen.rect(4.0, 3.0, 16.0, 18.0);
            pen.rect(7.0, 6.0, 10.0, 4.0);
            pen.fcircle(8.5, 14.0, 1.2);
            pen.fcircle(12.0, 14.0, 1.2);
            pen.fcircle(15.5, 14.0, 1.2);
            pen.fcircle(8.5, 18.0, 1.2);
            pen.fcircle(12.0, 18.0, 1.2);
            pen.fcircle(15.5, 18.0, 1.2);
        }
        Sheet => {
            pen.rect(4.0, 3.0, 16.0, 18.0);
            pen.line((4.0, 8.0), (20.0, 8.0));
            pen.line((11.0, 8.0), (11.0, 21.0));
            pen.line((4.0, 14.5), (20.0, 14.5));
        }
        Share => {
            pen.circle(17.0, 6.0, 2.6);
            pen.circle(6.0, 12.0, 2.6);
            pen.circle(17.0, 18.0, 2.6);
            pen.line((8.4, 10.7), (14.6, 7.3));
            pen.line((8.4, 13.3), (14.6, 16.7));
        }
        Fx => {
            pen.text("fx", 13.0);
        }
        Sum => {
            pen.text("Σ", 15.0);
        }
        Grid => {
            pen.rect(3.0, 3.0, 18.0, 18.0);
            for i in 1..3 {
                pen.line((3.0, 3.0 + i as f32 * 6.0), (21.0, 3.0 + i as f32 * 6.0));
                pen.line((3.0 + i as f32 * 6.0, 3.0), (3.0 + i as f32 * 6.0, 21.0));
            }
        }
        Lock => {
            pen.rect(6.0, 11.0, 18.0, 20.0);
            pen.line((8.0, 11.0), (8.0, 8.0));
            pen.line((8.0, 8.0), (12.0, 5.0));
            pen.line((12.0, 5.0), (16.0, 8.0));
            pen.line((16.0, 8.0), (16.0, 11.0));
        }
        Freeze => {
            pen.rect(3.0, 3.0, 21.0, 21.0);
            pen.line((3.0, 9.0), (21.0, 9.0));
            pen.line((9.0, 3.0), (9.0, 21.0));
            pen.rect(3.0, 3.0, 21.0, 9.0);
            pen.rect(3.0, 3.0, 9.0, 21.0);
        }
        Close => {
            pen.line((6.0, 6.0), (18.0, 18.0));
            pen.line((18.0, 6.0), (6.0, 18.0));
        }
        Plus => {
            pen.line((12.0, 5.0), (12.0, 19.0));
            pen.line((5.0, 12.0), (19.0, 12.0));
        }
        FontGrow => {
            pen.text("A", 11.0);
            pen.line((17.0, 18.0), (22.0, 18.0));
            pen.line((19.5, 15.5), (19.5, 20.5));
            pen.arrow_head((19.5, 14.0), (0.0, -1.0), 3.5);
        }
        FontShrink => {
            pen.text("A", 11.0);
            pen.line((17.0, 18.0), (22.0, 18.0));
            pen.line((19.5, 15.5), (19.5, 20.5));
            pen.arrow_head((19.5, 22.0), (0.0, 1.0), 3.5);
        }
        DecInc => {
            pen.text("0.0", 9.0);
            pen.line((18.5, 6.0), (22.5, 6.0));
            pen.line((20.5, 4.0), (20.5, 8.0));
        }
        DecDec => {
            pen.text("0.0", 9.0);
            pen.line((18.5, 6.0), (22.5, 6.0));
        }
        Refresh => {
            pen.circle(12.0, 12.0, 7.5);
            pen.arrow_head((17.0, 5.0), (1.0, -0.4), 5.0);
            pen.frect(15.0, 3.0, 4.0, 4.0);
        }
        Cf => {
            pen.rect(3.0, 4.0, 18.0, 16.0);
            pen.line((3.0, 8.5), (21.0, 8.5));
            pen.line((3.0, 14.0), (21.0, 14.0));
        }
    }
}
