import sys

p = 'crates/app/src/main.rs'
b = open(p, 'rb').read()

apply_old = ('        if apply {\r\n'
             '            let formula = fn_formula_from_args(&w.name, &w.args);\r\n').encode('utf-8')
if b.count(apply_old) == 1:
    eol = '\r\n'
else:
    apply_old = ('        if apply {\n'
                 '            let formula = fn_formula_from_args(&w.name, &w.args);\n').encode('utf-8')
    eol = '\n'
    if b.count(apply_old) != 1:
        sys.exit('apply anchor missing')

apply_new = (
    '        if apply {' + eol +
    '            // 回车确定后要把这个按键吃掉：同一帧稍后画的格内编辑器' + eol +
    '            // 会把它当成「提交并下移」，导致刚插进去的公式被立刻写死。' + eol +
    '            ctx.input_mut(|i| {' + eol +
    '                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);' + eol +
    '            });' + eol +
    '            let formula = fn_formula_from_args(&w.name, &w.args);' + eol
).encode('utf-8')
b = b.replace(apply_old, apply_new, 1)

# 搜索过滤后当前函数不在列表里 → 选第一个匹配项
marker = '在画控件之前读回车'.encode('utf-8')
i = b.find(marker)
if i < 0:
    sys.exit('marker missing')
j = b.rfind(b'let mut cancel = false;', 0, i)
if j < 0:
    sys.exit('cancel line missing')
k = b.rfind(b'\n', 0, j) + 1
if b'list.is_empty()' in b[k - 400:k + 400]:
    sys.exit('already inserted')
block = (
    '        if !list.is_empty() && !list.iter().any(|n| **n == w.name) {' + eol +
    '            // 搜索把当前函数过滤掉了：选中第一个匹配项' + eol +
    '            w.name = list[0].to_string();' + eol +
    '            w.args.clear();' + eol +
    '            w.scrolled = false;' + eol +
    '        }' + eol
).encode('utf-8')
b = b[:k] + block + b[k:]

open(p, 'wb').write(b)
print('ok')
