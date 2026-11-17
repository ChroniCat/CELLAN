//! 读一列文本，按 cmp_stroke 排序后写到 B 列（与 Excel xlStroke 对拍用）
use sheetcore::collate::cmp_stroke;
use sheetcore::{Cell, Value, xlsx};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, dst) = (a[1].clone(), a[2].clone());
    let mut wb = xlsx::load(std::path::Path::new(&src)).expect("open");
    let items: Vec<String> = {
        let sheet = &wb.sheets[0];
        let mut v = Vec::new();
        let mut r = 0u32;
        while let Some(c) = sheet.get(r, 0) {
            if c.value == Value::Empty {
                break;
            }
            v.push(c.value.display());
            r += 1;
        }
        v
    };
    let mut sorted = items.clone();
    sorted.sort_by(|x, y| cmp_stroke(x, y));
    {
        let sheet = &mut wb.sheets[0];
        for (i, s) in sorted.iter().enumerate() {
            sheet.set(
                i as u32,
                1,
                Cell { value: Value::Text(s.clone().into()), formula: None },
            );
        }
    }
    xlsx::save(std::path::Path::new(&dst), &wb).expect("save");
    println!("sorted {} to {}", sorted.len(), dst);
}
