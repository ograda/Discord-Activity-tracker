"""Regenerate DAT's original reminder ICO assets; Python standard library only.
Rasterize simple authored vector silhouettes at 4x for smooth 32/64px resources.
Not needed at build time or runtime.
"""
import math, struct
from pathlib import Path

def line(x,y,a,b,w):
    dx,dy=b[0]-a[0],b[1]-a[1]
    t=max(0,min(1,((x-a[0])*dx+(y-a[1])*dy)/(dx*dx+dy*dy)))
    return math.hypot(x-a[0]-t*dx,y-a[1]-t*dy)<w/2

def shape(name,x,y):
    circle=lambda cx,cy,r: math.hypot(x-cx,y-cy)<r
    if name=='water':
        return circle(.5,.65,.29) or (.17<y<.65 and abs(x-.5)<(y-.17)*.60)
    if name in ('stand_up','stretch'):
        paths=([((.5,.32),(.5,.62)),((.5,.40),(.25,.51)),((.5,.40),(.75,.51)),((.5,.62),(.32,.89)),((.5,.62),(.68,.89))]
            if name=='stand_up' else [((.5,.32),(.5,.62)),((.5,.4),(.23,.17)),((.5,.4),(.77,.17)),((.5,.62),(.25,.86)),((.5,.62),(.75,.86))])
        return circle(.5,.19,.095) or any(line(x,y,a,b,.085) for a,b in paths)
    if name=='eye_rest':
        outer=((x-.5)/.42)**2+((y-.5)/.26)**2
        inner=((x-.5)/.33)**2+((y-.5)/.17)**2
        return (outer<1 and inner>1) or circle(.5,.5,.10)
    # Bell silhouette with a cutout, hanger, and clapper.
    outer=circle(.5,.4,.24) or (.26<x<.74 and .4<y<.68)
    inner=circle(.5,.4,.15) or (.35<x<.65 and .4<y<.61)
    return (outer and not inner) or line(x,y,(.21,.71),(.79,.71),.085) or circle(.5,.84,.065) or circle(.5,.12,.045)

def dib(name,size):
    pixels=bytearray()
    for y in reversed(range(size)):
        for x in range(size):
            alpha=sum(shape(name,(x+(sx+.5)/4)/size,(y+(sy+.5)/4)/size) for sy in range(4) for sx in range(4))*255//16
            # Ember gradient, fixed asset identity in every palette.
            t=y/(size-1)
            pixels.extend((int(100-60*t),int(187-83*t),int(255-22*t),alpha))
    mask=bytes(((size+31)//32)*4*size)
    return struct.pack('<IiiHHIIiiII',40,size,size*2,1,32,0,len(pixels)+len(mask),0,0,0,0)+pixels+mask

for name in ('water','stand_up','stretch','eye_rest','generic'):
    images=[dib(name,n) for n in (32,64)]
    offset=6+16*len(images)
    entries=bytearray()
    for size,data in zip((32,64),images):
        entries.extend(struct.pack('<BBBBHHII',size,size,0,0,1,32,len(data),offset))
        offset+=len(data)
    Path(__file__).with_name(name+'.ico').write_bytes(struct.pack('<HHH',0,1,len(images))+entries+b''.join(images))
