import sys, pypdfium2 as pdfium
TOKENS=["2026 年第一季度销售汇总","产品","铅笔","¥1,234,567.89","--987.65","25.6%"]
def boxes(path):
    doc=pdfium.PdfDocument(path); out=[]
    for i in range(len(doc)):
        tp=doc[i].get_textpage(); t=tp.get_text_range()
        d={}
        for tok in TOKENS:
            k=t.find(tok)
            if k<0: d[tok]=None; continue
            l,b,r,tt=tp.get_charbox(k)
            d[tok]=(round(l,1),round(b,1))
        out.append(d)
    return out
a=boxes(sys.argv[1]); b=boxes(sys.argv[2])
for i in range(max(len(a),len(b))):
    print(f'page {i+1}')
    da=a[i] if i<len(a) else {}; db=b[i] if i<len(b) else {}
    for tok in TOKENS:
        pa=da.get(tok); pb=db.get(tok)
        if pa is None or pb is None:
            print(f'  {tok!r}: ours={pa} excel={pb}')
        else:
            dx=abs(pa[0]-pb[0]); dy=abs(pa[1]-pb[1])
            flag='OK' if dx<=3 and dy<=3 else 'DIFF'
            print(f'  {tok!r}: ours={pa} excel={pb} d=({dx:.1f},{dy:.1f}) {flag}')
