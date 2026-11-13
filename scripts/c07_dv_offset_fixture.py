from openpyxl import Workbook
from openpyxl.worksheet.datavalidation import DataValidation

out = r"F:\finance-native-sheets-ready\crates\core\tests\fixtures\c07\offset.xlsx"
wb = Workbook()
main = wb.active
main.title = "主表"
data = wb.create_sheet("数据")

for i, v in enumerate(["甲", "乙", "丙", "丁"], start=1):
    main.cell(row=i, column=1, value=v)
for i, v in enumerate([100, 200, 300, 400, 500], start=1):
    data.cell(row=i, column=1, value=v)
main["B1"] = 1
main["B2"] = 2

dv1 = DataValidation(type="list", formula1="OFFSET($A$1:$A$3,0,0)", allow_blank=False)
main.add_data_validation(dv1)
dv1.add("C1:C2")

dv2 = DataValidation(
    type="list", formula1="OFFSET(数据!$A$1,0,0,COUNTA($B:$B),1)", allow_blank=False
)
main.add_data_validation(dv2)
dv2.add("D1")

dv3 = DataValidation(type="list", formula1="OFFSET($A$3,0,0,-2,1)", allow_blank=False)
main.add_data_validation(dv3)
dv3.add("E1")

wb.save(out)
print("SAVED", out)
