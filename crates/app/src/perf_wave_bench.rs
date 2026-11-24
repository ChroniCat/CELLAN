//! Wave 0-4 App 端性能基线套件（隔离 TestEnv，默认 `#[ignore]` 不影响常规 `cargo test`）。
//!
//! 运行（release + offline）：
//!   cargo test --release --offline -p app --no-default-features --features glow --bin app perf_wave_bench -- --ignored --nocapture --test-threads=1
use super::*;
use sheetcore::charts::{Chart, ChartKind, LabelOpts, Series};
use sheetcore::style::CellStyle;
use std::hint::black_box;
use std::time::Instant;

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn emit(name: &str, value: serde_json::Value) {
    println!(
        "PERF_WAVE_APP_JSON: {}",
        serde_json::json!({ "bench": name, "metrics": value })
    );
}

fn run_frame_1440(app: &mut SheetApp, ctx: &egui::Context) -> egui::FullOutput {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    ctx.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
}

#[test]
#[ignore = "Wave 0-4 performance benchmark suite (run explicitly with --ignored)"]
fn perf_wave_app_suite() {
    let _env = crate::test_env::TestEnv::new("perf-wave-app-bench");
    crate::clear_test_clipboard();

    // 1) Cold startup (SheetApp::new_with_context including install_fonts / UI system fonts)
    let ctx = egui::Context::default();
    let t_boot = Instant::now();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let cold_init_ms = ms(t_boot.elapsed());
    app.show_start = false;

    // Populate a realistic 500x20 grid so the viewport is dense
    {
        let sh = app.wb.sheet_mut(0);
        for r in 0..500u32 {
            sh.set(r, 0, Cell { value: Value::Text(format!("科目-{r:04}").into()), formula: None });
            for c in 1..20u32 {
                sh.set(r, c, Cell { value: Value::Number((r * 100 + c) as f64), formula: None });
            }
        }
    }

    // Warmup 3 frames
    for _ in 0..3 {
        let _ = run_frame_1440(&mut app, &ctx);
    }

    // 2) Idle repaint check + 1440px full UI frame throughput (Ribbon mid_tier + Grid Tier-2)
    let idle_out = run_frame_1440(&mut app, &ctx);
    let idle_repaint_ms = idle_out
        .viewport_output
        .values()
        .next()
        .map(|v| v.repaint_delay.as_secs_f64() * 1000.0)
        .unwrap_or(f64::INFINITY);

    let frames = 100usize;
    let t_ui = Instant::now();
    for _ in 0..frames {
        let out = run_frame_1440(&mut app, &ctx);
        black_box(out.shapes.len());
    }
    let ui_100frames_ms = ms(t_ui.elapsed());

    emit(
        "app_startup_and_1440_frames",
        serde_json::json!({
            "cold_init_ms": (cold_init_ms * 100.0).round() / 100.0,
            "idle_repaint_delay_ms": if idle_repaint_ms.is_finite() { (idle_repaint_ms * 10.0).round() / 10.0 } else { -1.0 },
            "frames_100_ms": (ui_100frames_ms * 100.0).round() / 100.0,
            "avg_frame_ms": ((ui_100frames_ms / frames as f64) * 1000.0).round() / 1000.0,
        }),
    );

    // 3) Single-cell edit invalidation cost across 30 consecutive single-cell edits
    let edits = 30u32;
    let t_edit = Instant::now();
    for i in 0..edits {
        app.wb.sheet_mut(0).set(2, 2, Cell { value: Value::Number(9000.0 + i as f64), formula: None });
        app.doc_gen = app.doc_gen.wrapping_add(1);
        let out = run_frame_1440(&mut app, &ctx);
        black_box(out.shapes.len());
    }
    let edit_30frames_ms = ms(t_edit.elapsed());
    emit(
        "app_grid_single_edit_30frames",
        serde_json::json!({
            "edits": edits,
            "total_ms": (edit_30frames_ms * 100.0).round() / 100.0,
            "avg_frame_ms": ((edit_30frames_ms / edits as f64) * 1000.0).round() / 1000.0,
        }),
    );

    // 4) Chart scroll 100 frames (5 charts x 50k points) + 4x chart_at_sel per frame
    let n_pts = 50_000usize;
    let vals: Vec<f64> = (0..n_pts).map(|i| ((i as f64) * 0.01).sin() * 100.0).collect();
    let cats: Vec<String> = (0..n_pts).map(|i| format!("P{i}")).collect();
    let base_chart = Chart {
        kind: ChartKind::Line,
        title: "Perf Line".into(),
        series: vec![Series {
            name: "Series1".into(),
            cats,
            vals,
            ..Default::default()
        }],
        legend: true,
        anchor: (1, 1, 15, 8),
        legend_pos: "r".into(),
        part: "xl/charts/chart1.xml".into(),
        drawing: "xl/drawings/drawing1.xml".into(),
        show_labels: false,
        label_opts: LabelOpts::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: "X".into(),
        axis_y: "Y".into(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    };
    app.wb.sheet_mut(0).charts = (0..5).map(|_| base_chart.clone()).collect();
    app.chart_sel = Some(0);

    let mut chart_shapes_ms = 0.0;
    let mut chart_sel_ms = 0.0;
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
        ..Default::default()
    };
    let fonts_gen = system_fonts::generation();
    let _ = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let painter = ui.painter();
            let t_s = Instant::now();
            for frame_i in 0..50usize {
                for ci in 0..5usize {
                    let y = 40.0 + (ci as f32) * 180.0 - (frame_i as f32) * 3.0;
                    let rect = egui::Rect::from_min_size(egui::pos2(80.0, y), egui::vec2(420.0, 240.0));
                    let sh = app.wb.sheet(0);
                    let ch = &sh.charts[ci];
                    let shapes = app.cf_inputs.chart_shapes(
                        painter,
                        sh,
                        app.doc_gen,
                        app.wb_epoch,
                        fonts_gen,
                        ci,
                        rect,
                        ch,
                    );
                    black_box(shapes.len());
                }
            }
            chart_shapes_ms = ms(t_s.elapsed());

            let t_sel = Instant::now();
            for _ in 0..200 {
                // 50 frames x 4 calls per frame
                let ch = app.chart_at_sel();
                black_box(ch.is_some());
            }
            chart_sel_ms = ms(t_sel.elapsed());
        });
    });
    app.wb.sheet_mut(0).charts.clear();
    app.chart_sel = None;

    emit(
        "app_chart_scroll_50frames",
        serde_json::json!({
            "charts": 5,
            "points_per_chart": n_pts,
            "chart_shapes_50frames_ms": (chart_shapes_ms * 100.0).round() / 100.0,
            "chart_at_sel_200calls_ms": (chart_sel_ms * 100.0).round() / 100.0,
        }),
    );

    // 5) AI set_cells footprint refresh on a 1,500-style workbook (500 cell edits)
    for i in 0..1500u32 {
        let mut s = CellStyle::default();
        s.fill = Some([(i % 256) as u8, ((i * 3) % 256) as u8, ((i * 7) % 256) as u8]);
        s.bold = i % 2 == 0;
        s.format = format!("#,##0.{i}");
        app.wb.styles.intern(s);
    }
    let ai_fp_ms = ms(app.bench_ai_set_cells_footprint(500));
    emit(
        "app_ai_set_cells_footprint_500x1500styles",
        serde_json::json!({
            "styles": app.wb.styles.len(),
            "edits": 500,
            "footprint_refresh_ms": (ai_fp_ms * 100.0).round() / 100.0,
        }),
    );

    // 6) Collab HostWatermark 100k clone + 20-peer 64KB broadcast seal
    let (wm_clone_ms, seal_20peers_ms, wire_bytes) = crate::net::bench_collab_hotpaths(100_000, 20);
    emit(
        "app_collab_watermark_and_broadcast",
        serde_json::json!({
            "watermark_entries": 100_000,
            "watermark_clone_ms": (wm_clone_ms * 100.0).round() / 100.0,
            "broadcast_20peers_64kb_ms": (seal_20peers_ms * 100.0).round() / 100.0,
            "total_wire_kb": ((wire_bytes as f64 / 1024.0) * 10.0).round() / 10.0,
        }),
    );
}
