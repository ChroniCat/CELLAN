"""Run synthetic in-memory comparisons in installed Excel and WPS; never open user files.
Usage: python run_office_oracle.py --out <new-output.json>
Requires pywin32. WPS exposes ket.Application on this audited machine.
"""
import argparse
import json
from pathlib import Path
import win32com.client

parser = argparse.ArgumentParser()
parser.add_argument('--out', required=True)
args = parser.parse_args()
out = Path(args.out).resolve()
if out.exists():
    raise SystemExit('Choose a new output path; refusing to overwrite an existing file.')
out.parent.mkdir(parents=True, exist_ok=True)
results = {}
for product, progid in [('excel', 'Excel.Application'), ('wps', 'ket.Application')]:
    app = None
    books = []
    try:
        app = win32com.client.DispatchEx(progid)
        # Do not change user-global display/security preferences or quit shared WPS.
        book = app.Workbooks.Add()
        books.append(book)
        sheet = book.Worksheets(1)
        item = {'version': app.Version, 'formulas': {}}
        for formula in ['=DAY(60)', '=TEXT(60,"yyyy-mm-dd")', '=SEQUENCE(2,2)+1',
                        '=SEQUENCE(3,1)*2', '=SUM({1;2}+{10,20})', '="_xlfn.TEXT"']:
            try:
                item['formulas'][formula] = app.Evaluate(formula)
            except Exception as error:
                item['formulas'][formula] = {'error': str(error)}
        for formula in ['=A1=0', '=A1=""', '=A1<0']:
            sheet.Range('B1').Formula = formula
            sheet.Calculate()
            item['formulas'][formula] = sheet.Range('B1').Value2
        sheet.Range('A1').Formula = '=1/0'
        sheet.Range('B1').Value2 = 1
        sheet.Range('C1').Formula = '=SUM(A1,B1)'
        sheet.Calculate()
        item['incremental_before'] = sheet.Range('C1').Text
        sheet.Range('B1').Value2 = 2
        sheet.Calculate()
        item['incremental_after'] = sheet.Range('C1').Text
        date_book = app.Workbooks.Add()
        books.append(date_book)
        date_book.Date1904 = True
        date_sheet = date_book.Worksheets(1)
        date_sheet.Range('A1').Value2 = 1
        date_sheet.Range('A1').NumberFormat = 'yyyy-mm-dd'
        item['date1904_A1_iso'] = date_sheet.Range('A1').Text
        results[product] = item
    except Exception as error:
        results[product] = {'error': str(error)}
    finally:
        for book in reversed(books):
            try:
                book.Close(False)
            except Exception:
                pass
    out.write_text(json.dumps(results, ensure_ascii=False, indent=2, default=str), encoding='utf-8')
print(json.dumps(results, ensure_ascii=False, indent=2, default=str))
