"""用最小二乘把我们的 PDF 拟合到 Excel PDF（整体缩放+平移），看残差。

用法: python c16_pdf_fit.py ours.pdf excel.pdf
"""
import sys, statistics
import numpy as np
import pypdfium2 as pdfium

TOK = ["铅笔","笔记本","橡皮","行10","行11","行12","180.00","360.00","160.00",
       "¥1,234,567.89","--987.65","25.6%","产品","数量","单价","金额"]

def boxes(path):
    out={}
    doc=pdfium.PdfDocument(path)
    for i in range(len(doc)):
        tp=doc[i].get_textpage(); t=tp.get_text_range()
        for tok in TOK:
            k=t.find(tok)
            if k>=0 and tok not in out:
                l,b,r,tt=tp.get_charbox(k)
                l2,b2,r2,t2=tp.get_charbox(k+len(tok)-1)
                out[tok]=(l,b,r2,tt,i)
    return out

A,B=boxes(sys.argv[1]),boxes(sys.argv[2])
xs=[(A[k][0],B[k][0]) for k in TOK if k in A and k in B]
ys=[(A[k][1],B[k][1]) for k in TOK if k in A and k in B]
def fit(pairs):
    a=np.array([[p[0],1.0] for p in pairs]); b=np.array([p[1] for p in pairs])
    (s,c),res,*_=np.linalg.lstsq(a,b,rcond=None)
    resid=[y-(s*x+c) for x,y in pairs]
    return s,c,resid
sx,cx,rx=fit(xs); sy,cy,ry=fit(ys)
print(f"n={len(xs)}  x: s={sx:.4f} c={cx:+.2f} max|res|={max(map(abs,rx)):.2f} rms={statistics.pstdev(rx):.2f}")
print(f"          y: s={sy:.4f} c={cy:+.2f} max|res|={max(map(abs,ry)):.2f} rms={statistics.pstdev(ry):.2f}")
for (tok,(ax,ay)),rxv,ryv in zip([(k,(A[k][0],A[k][1])) for k in TOK if k in A and k in B],rx,ry):
    print(f"  {tok:14s} res=({rxv:+6.2f},{ryv:+6.2f})")
