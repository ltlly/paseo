import ctypes as C,sys,time
x=C.CDLL('libX11.so.6');xt=C.CDLL('libXtst.so.6');x.XOpenDisplay.argtypes=[C.c_char_p];x.XOpenDisplay.restype=C.c_void_p;d=x.XOpenDisplay(b':99');assert d
x.XDefaultRootWindow.argtypes=[C.c_void_p];x.XDefaultRootWindow.restype=C.c_ulong;root=x.XDefaultRootWindow(d)
x.XQueryTree.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.POINTER(C.c_ulong)),C.POINTER(C.c_uint)];x.XFetchName.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(C.c_void_p)];x.XFree.argtypes=[C.c_void_p]
found=[]
def walk(parent,depth=0):
 r=C.c_ulong();p=C.c_ulong();ws=C.POINTER(C.c_ulong)();n=C.c_uint()
 if not x.XQueryTree(d,parent,C.byref(r),C.byref(p),C.byref(ws),C.byref(n)):return
 children=[ws[i] for i in range(n.value)];x.XFree(ws)
 for child in children:
  namep=C.c_void_p();x.XFetchName(d,child,C.byref(namep));name=C.string_at(namep) if namep.value else b'';x.XFree(namep)
  if name==b'Paseo Native':found.append(child)
  if depth<6:walk(child,depth+1)
walk(root);assert len(found)==1,found;w=found[0]
x.XSetInputFocus.argtypes=[C.c_void_p,C.c_ulong,C.c_int,C.c_ulong];x.XFlush.argtypes=[C.c_void_p];x.XKeysymToKeycode.argtypes=[C.c_void_p,C.c_ulong];x.XStringToKeysym.argtypes=[C.c_char_p];x.XStringToKeysym.restype=C.c_ulong
xt.XTestFakeMotionEvent.argtypes=[C.c_void_p,C.c_int,C.c_int,C.c_int,C.c_ulong];xt.XTestFakeButtonEvent.argtypes=[C.c_void_p,C.c_uint,C.c_int,C.c_ulong];xt.XTestFakeKeyEvent.argtypes=[C.c_void_p,C.c_uint,C.c_int,C.c_ulong]
class Attr(C.Structure):
 _fields_=[('x',C.c_int),('y',C.c_int),('width',C.c_int),('height',C.c_int),('border_width',C.c_int),('depth',C.c_int),('visual',C.c_void_p),('root',C.c_ulong),('class_',C.c_int),('bit_gravity',C.c_int),('win_gravity',C.c_int),('backing_store',C.c_int),('backing_planes',C.c_ulong),('backing_pixel',C.c_ulong),('save_under',C.c_int),('colormap',C.c_ulong),('map_installed',C.c_int),('map_state',C.c_int),('all_event_masks',C.c_long),('your_event_mask',C.c_long),('do_not_propagate_mask',C.c_long),('override_redirect',C.c_int),('screen',C.c_void_p)]
x.XGetWindowAttributes.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(Attr)]
for _ in range(50):
 attr=Attr();assert x.XGetWindowAttributes(d,w,C.byref(attr))
 if attr.map_state==2 and attr.width>100:break
 time.sleep(.1)
else:raise RuntimeError('Private native window is not viewable')
if sys.argv[1] in ['click','right','type','wheel','key','drag']:x.XSetInputFocus(d,w,2,0)
def button(b):xt.XTestFakeButtonEvent(d,b,1,0);xt.XTestFakeButtonEvent(d,b,0,0);x.XFlush(d)
if sys.argv[1] in ['click','right']:xt.XTestFakeMotionEvent(d,-1,int(sys.argv[2]),int(sys.argv[3]),0);button(3 if sys.argv[1]=='right' else 1)
if sys.argv[1]=='type':
 for ch in sys.argv[2]:
  sym=x.XStringToKeysym(('space' if ch==' ' else ch).encode()) or ord(ch);code=x.XKeysymToKeycode(d,sym);assert code;shift=x.XKeysymToKeycode(d,x.XStringToKeysym(b'Shift_L'));upper=ch.isascii()and ch.isupper();xt.XTestFakeKeyEvent(d,shift,1,0)if upper else None;xt.XTestFakeKeyEvent(d,code,1,0);xt.XTestFakeKeyEvent(d,code,0,0);xt.XTestFakeKeyEvent(d,shift,0,0)if upper else None;x.XFlush(d);time.sleep(.02)
if sys.argv[1]=='wheel':
 xt.XTestFakeMotionEvent(d,-1,750,400,0)
 for i in range(int(sys.argv[2])):button(5)
if sys.argv[1]=='close':
 class Data(C.Union):_fields_=[('b',C.c_char*20),('s',C.c_short*10),('l',C.c_long*5)]
 class Msg(C.Structure):_fields_=[('type',C.c_int),('serial',C.c_ulong),('send_event',C.c_int),('display',C.c_void_p),('window',C.c_ulong),('message_type',C.c_ulong),('format',C.c_int),('data',Data)]
 class Event(C.Union):_fields_=[('msg',Msg),('padding',C.c_long*24)]
 x.XInternAtom.argtypes=[C.c_void_p,C.c_char_p,C.c_int];x.XInternAtom.restype=C.c_ulong;x.XSendEvent.argtypes=[C.c_void_p,C.c_ulong,C.c_int,C.c_long,C.POINTER(Event)]
 e=Event();e.msg.type=33;e.msg.display=d;e.msg.window=w;e.msg.message_type=x.XInternAtom(d,b'WM_PROTOCOLS',0);e.msg.format=32;e.msg.data.l[0]=x.XInternAtom(d,b'WM_DELETE_WINDOW',0);x.XSendEvent(d,w,0,0,C.byref(e));x.XFlush(d)

if sys.argv[1]=='key':
 codes=[x.XKeysymToKeycode(d,x.XStringToKeysym(k.encode())) for k in sys.argv[2:]]
 assert all(codes)
 for code in codes:xt.XTestFakeKeyEvent(d,code,1,0)
 for code in reversed(codes):xt.XTestFakeKeyEvent(d,code,0,0)
 x.XFlush(d)

if sys.argv[1]=='drag':
 sx,sy,ex,ey=map(int,sys.argv[2:]);xt.XTestFakeMotionEvent(d,-1,sx,sy,0);xt.XTestFakeButtonEvent(d,1,1,0);x.XFlush(d);time.sleep(.1)
 for step in range(1,11):xt.XTestFakeMotionEvent(d,-1,int(sx+(ex-sx)*step/10),int(sy+(ey-sy)*step/10),0);x.XFlush(d);time.sleep(.02)
 xt.XTestFakeButtonEvent(d,1,0,0);x.XFlush(d)

if sys.argv[1]=='resize':
 x.XResizeWindow.argtypes=[C.c_void_p,C.c_ulong,C.c_uint,C.c_uint];x.XResizeWindow(d,w,int(sys.argv[2]),int(sys.argv[3]));x.XFlush(d)

if sys.argv[1]=='hover':
 xt.XTestFakeMotionEvent(d,-1,int(sys.argv[2]),int(sys.argv[3]),0);x.XFlush(d)
if sys.argv[1]=='middle':
 xt.XTestFakeMotionEvent(d,-1,int(sys.argv[2]),int(sys.argv[3]),0);button(2)

if sys.argv[1]=='hwheel':
 xt.XTestFakeMotionEvent(d,-1,750,154,0)
 for _ in range(abs(int(sys.argv[2]))):button(6 if int(sys.argv[2])<0 else 7)

if sys.argv[1]=='dockwheel':
 xt.XTestFakeMotionEvent(d,-1,1200,117,0)
 for _ in range(abs(int(sys.argv[2]))):button(6 if int(sys.argv[2])<0 else 7)

if sys.argv[1]=='draghold':
 sx,sy,ex,ey=map(int,sys.argv[2:]);x.XSetInputFocus(d,w,2,0);xt.XTestFakeMotionEvent(d,-1,sx,sy,0);xt.XTestFakeButtonEvent(d,1,1,0);x.XFlush(d);time.sleep(.1)
 for step in range(1,11):xt.XTestFakeMotionEvent(d,-1,int(sx+(ex-sx)*step/10),int(sy+(ey-sy)*step/10),0);x.XFlush(d);time.sleep(.02)
if sys.argv[1]=='release':xt.XTestFakeButtonEvent(d,1,0,0);x.XFlush(d)

if sys.argv[1] in ['dragescape','fastdrag']:
 sx,sy,ex,ey=map(int,sys.argv[2:]);x.XSetInputFocus(d,w,2,0);xt.XTestFakeMotionEvent(d,-1,sx,sy,0);xt.XTestFakeButtonEvent(d,1,1,0);x.XFlush(d);time.sleep(.02)
 for step in range(1,6):xt.XTestFakeMotionEvent(d,-1,int(sx+(ex-sx)*step/5),int(sy+(ey-sy)*step/5),0);x.XFlush(d);time.sleep(.015)
 if sys.argv[1]=='dragescape':
  time.sleep(.15)
  for _ in range(3):button(5)
  time.sleep(.15);code=x.XKeysymToKeycode(d,x.XStringToKeysym(b'Escape'));xt.XTestFakeKeyEvent(d,code,1,0);xt.XTestFakeKeyEvent(d,code,0,0);x.XFlush(d);time.sleep(.15)
 xt.XTestFakeButtonEvent(d,1,0,0);x.XFlush(d);time.sleep(.05)
