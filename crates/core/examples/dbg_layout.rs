// 调试：打印列宽行高的像素布局（无界面也能验证）
fn main() {
    let path = std::env::args().nth(1).expect("用法: dbg_layout <xlsx>");
    let wb = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("打开失败");
    for sh in &wb.sheets {
        println!("表 {}  自定义列 {}  自定义行 {}  合并 {}",
            sh.name, sh.cols.len(), sh.rows.len(), sh.merges.len());
        let n = 8u32.min(200);
        print!("  列宽: ");
        for c in 0..n {
            print!("{}={:.0} ", (b'A' + c as u8) as char, sh.cols.size(c));
        }
        println!();
        print!("  列起点: ");
        for c in 0..n {
            print!("{} ", sh.cols.pos(c) as i32);
        }
        println!();
        print!("  行高: ");
        for r in 0..6 {
            print!("{}={:.0} ", r + 1, sh.rows.size(r));
        }
        println!();
        print!("  行起点: ");
        for r in 0..6 {
            print!("{} ", sh.rows.pos(r) as i32);
        }
        println!();
        // 命中测试自检
        let mut ok = true;
        for c in 0..n {
            let px = sh.cols.pos(c) + sh.cols.size(c) * 0.5;
            if sh.cols.index_at(px) != c {
                println!("  !! 命中错误: x={:.0} 期望列 {c} 实际 {}", px, sh.cols.index_at(px));
                ok = false;
            }
        }
        println!("  命中测试: {}", if ok { "全部正确" } else { "有误" });
    }
}
