import sys, pypdfium2 as pdfium
from PIL import Image, ImageChops
def render(path, scale=2.0):
    doc = pdfium.PdfDocument(path)
    out=[]
    for i in range(len(doc)):
        page = doc[i]
        bmp = page.render(scale=scale)
        out.append((page.get_size(), bmp.to_pil().convert('L')))
    return out
def ink(img):
    px=img.load(); w,h=img.size; n=0
    for y in range(0,h,3):
        for x in range(0,w,3):
            if px[x,y] < 235: n+=1
    return n/float((w//3+1)*(h//3+1))
def main(ours, excel):
    a=render(ours); b=render(excel)
    print(f'pages ours={len(a)} excel={len(b)}')
    for i,(pa,ia) in enumerate(a):
        if i>=len(b):
            print(f'p{i+1} MISSING in excel'); continue
        pb,ib=b[i]
        sa=ImageChops.difference(ia, ib).resize((600, int(600*ia.size[1]/ia.size[0]))).resize(ia.size)
        import numpy as np
        da=np.asarray(ia.resize((400,int(400*ia.size[1]/ia.size[0]))),dtype=np.int16)
        db=np.asarray(ib.resize((400,int(400*ib.size[1]/ib.size[0]))),dtype=np.int16)
        mae=float(abs(da-db).mean())
        print(f'p{i+1} size ours={pa[0]:.0f}x{pa[1]:.0f} excel={pb[0]:.0f}x{pb[1]:.0f} ink={ink(ia):.3f}/{ink(ib):.3f} mae={mae:.1f}')
    # 文本对照
    da=pdfium.PdfDocument(ours); db=pdfium.PdfDocument(excel)
    for i in range(len(da)):
        t1=da[i].get_textpage().get_text_range().replace('\r','')
        t2=db[i].get_textpage().get_text_range().replace('\r','') if i < len(db) else ''
        same = (t1==t2)
        print(f'p{i+1} text same={same} ours_len={len(t1)} excel_len={len(t2)}')
        if not same:
            import difflib
            d=[l for l in difflib.unified_diff(t2.splitlines(), t1.splitlines(), 'excel','ours', lineterm='')][:14]
            print('\n'.join(d))
main(sys.argv[1], sys.argv[2])
