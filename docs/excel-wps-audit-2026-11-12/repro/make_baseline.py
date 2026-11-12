"""Generate the synthetic visual baseline with installed Excel; no existing file is opened."""
import argparse
from pathlib import Path
import win32com.client

parser = argparse.ArgumentParser()
parser.add_argument('--out', required=True)
target = Path(parser.parse_args().out).resolve()
if target.exists():
    raise SystemExit('Output already exists; choose a new path.')
target.parent.mkdir(parents=True, exist_ok=True)
excel = win32com.client.DispatchEx('Excel.Application')
book = excel.Workbooks.Add()
try:
    book.Date1904 = False
    sh = book.Worksheets(1)
    sh.Name = '审计样表'
    sh.Range('A1:H9').Value = (
        ('部门', '项目', '数量', '单价', '金额', '完成率', '日期', '备注'),
        ('研发', '项目甲', 2, 1234.5, None, .35, 46338, '中文与 Latin ABC'),
        ('生产', '项目乙', 3, 68.8, None, .8, 46339, '一段较长的中文说明，用于检验文本裁剪与自动换行'),
        ('销售', '项目丙', 5, 12, None, 1., 46340, '前导零编码 00123'),
        ('研发', '项目丁', 1, -50, None, 0., 46341, '负数和零值'),
        ('生产', '项目戊', 0, 120, None, .15, 46342, '含空白与错误测试'),
        ('销售', '项目己', 7, 99.99, None, .65, 46343, '纯合成数据'),
        ('合计', None, None, None, None, None, None, None),
        ('测试', 1, 2, '=B9*10', '=C9*10', '=SUM(D9:E9)', None, '删除 B9:C9 应重算 D9:F9'))
    sh.Range('E2:E7').FormulaR1C1 = '=RC[-2]*RC[-1]'
    sh.Range('E8').Formula = '=SUM(E2:E7)'
    sh.Range('A1:H9').Font.Name = '等线'
    sh.Range('A1:H9').Font.Size = 11
    sh.Range('A1:H1').Interior.Color = 0xE9E1D9
    sh.Range('A1:H1').Font.Bold = True
    sh.Range('A1:H1').RowHeight = 25
    sh.Range('A2:H9').RowHeight = 24
    for col, width in [('A', 12), ('B', 16), ('C', 10), ('D', 14),
                       ('E', 16), ('F', 12), ('G', 16), ('H', 36)]:
        sh.Columns(col).ColumnWidth = width
    sh.Range('D2:E8').NumberFormat = '#,##0.00'
    sh.Range('F2:F7').NumberFormat = '0%'
    sh.Range('G2:G7').NumberFormat = 'yyyy-mm-dd'
    sh.Range('A8:H8').Font.Bold = True
    sh.Range('B2').Select()
    excel.ActiveWindow.Zoom = 100
    sh.Calculate()
    book.SaveAs(str(target), 51)
    print(target)
finally:
    book.Close(False)
    # Only quit if this dedicated instance has no other workbooks.
    if excel.Workbooks.Count == 0:
        excel.Quit()
