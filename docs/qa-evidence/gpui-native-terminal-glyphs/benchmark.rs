use paseo_desktop::terminal::{GridSize, TerminalBuffer};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    time::Instant,
};
struct Count;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ENABLED.load(Relaxed) {
            FREED.fetch_add(l.size(), Relaxed);
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(n, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOC: Count = Count;

use gpui::{ShapedLine, TextRun, font, px, rgb};
mod glyphs {
    include!("/home/ltlly/Code/paseo/packages/desktop/native/src/terminal_view/glyphs.rs");
    impl Glyphs {
        pub fn stats(&self) -> (usize, usize) {
            (self.entries.len(), self.text_bytes)
        }
    }
}

fn original(cell: &paseo_desktop::terminal::Cell<'_>, ts: &gpui::WindowTextSystem) -> ShapedLine {
    let base = font("JetBrains Mono");
    let face = match (cell.bold, cell.italic) {
        (true, true) => base.bold().italic(),
        (true, false) => base.bold(),
        (false, true) => base.italic(),
        _ => base,
    };
    let text: gpui::SharedString = if cell.zerowidth.is_empty() {
        cell.character.into()
    } else {
        cell.chars().collect::<String>().into()
    };
    let color = rgb(if cell.selected {
        0xfafafa
    } else {
        cell.foreground
    })
    .into();
    let run = TextRun {
        len: text.len(),
        font: face,
        color,
        background_color: None,
        underline: cell.underline.then_some(gpui::UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: None,
    };
    ts.shape_line(text, px(14.), &[run], None)
}
fn main() {
    gpui_platform::application().run(|cx|{
 use std::borrow::Cow;
 let path="/home/ltlly/Code/paseo/packages/app/modules/paseo-terminal-fonts/android/src/main/assets/paseo-terminal-fonts";
 let faces=["Regular","Bold","Italic","BoldItalic"].map(|name|Cow::Owned(std::fs::read(format!("{path}/JetBrainsMono-{name}.ttf")).unwrap()));cx.text_system().add_fonts(faces.to_vec()).unwrap();
 let ts=gpui::WindowTextSystem::new(cx.text_system().clone());
 for (name,text) in [("ascii","0123456789 abcdefghijklmnopqrstuvwxyz ".repeat(1000)),("unicode","中e\u{301}👩\u{200d}💻한글नमस्तेالعربية | ".repeat(500)),("styles","\x1b[1;3;4;38;2;105;200;123m0123 e\u{301} 中文\x1b[0m | ".repeat(500))]{
  let mut buffer=TerminalBuffer::new(GridSize{rows:40,cols:120},100);buffer.feed(text.as_bytes());let mut cache=glyphs::Glyphs::default();cache.prepare(font("JetBrains Mono"),px(14.));
  for cell in buffer.screen().cells.filter(|c|c.character!=' '||!c.zerowidth.is_empty()) {let expected=original(&cell,&ts);let got=cache.line(&cell,&ts,0xfafafa);assert_eq!(format!("{expected:?}"),format!("{got:?}"));}
  for cached in [false,true] {
   let mut checksum=0;CALLS.store(0,Relaxed);BYTES.store(0,Relaxed);ENABLED.store(true,Relaxed);let start=Instant::now();
   for _ in 0..100 {for cell in buffer.screen().cells.filter(|c|c.character!=' '||!c.zerowidth.is_empty()) {let line=if cached {cache.line(&cell,&ts,0xfafafa)}else{Cow::Owned(original(&cell,&ts))};checksum+=black_box(line.len());}}
   let ns=start.elapsed().as_nanos();ENABLED.store(false,Relaxed);println!("name={name} cached={cached} iterations=100 nsPerTraversal={} allocsPerTraversal={} allocatedBytesPerTraversal={} cache={:?} checksum={checksum}",ns/100,CALLS.load(Relaxed)/100,BYTES.load(Relaxed)/100,cache.stats());
  }
 }
 let mut cache=glyphs::Glyphs::default();cache.prepare(font("JetBrains Mono"),px(14.));let mut buffer=TerminalBuffer::new(GridSize{rows:24,cols:80},0);
 for i in 0..1000 {buffer.feed(format!("\x1b[H\x1b[38;2;{};{};{}m{}",i%256,(i/256)%256,22,char::from_u32(0x4e00+i).unwrap()).as_bytes());let cell=buffer.screen().cells.next().unwrap();assert_eq!(format!("{:?}",original(&cell,&ts)),format!("{:?}",cache.line(&cell,&ts,0xfafafa)));assert!(cache.stats().0<=256 && cache.stats().1<=16*1024);}
 buffer.feed(format!("\x1b[Hlong{}","\u{301}".repeat(600)).as_bytes());for cell in buffer.screen().cells.take(4) {assert_eq!(format!("{:?}",original(&cell,&ts)),format!("{:?}",cache.line(&cell,&ts,0xfafafa)));}

 let count=cache.stats().0;FREED.store(0,Relaxed);ENABLED.store(true,Relaxed);cache.clear();ENABLED.store(false,Relaxed);assert_eq!(cache.stats(),(0,0));assert!(FREED.load(Relaxed)>=count*std::mem::size_of::<ShapedLine>());println!("Cleared entries={count}, freed requested allocation bytes={}",FREED.load(Relaxed));
 for i in 0..100 {buffer.feed(format!("\x1b[2J\x1b[H{}{}",char::from_u32(0x5000+i).unwrap(),"\u{301}".repeat(200)).as_bytes());let cell=buffer.screen().cells.next().unwrap();assert_eq!(format!("{:?}",original(&cell,&ts)),format!("{:?}",cache.line(&cell,&ts,0xfafafa)));assert!(cache.stats().0<256 && cache.stats().1<=16*1024);}
 assert!(cache.stats().1>12*1024);println!("Text-byte budget bounded while shaping full combining sequences: {:?}",cache.stats());
 buffer.begin_selection(0,0,false);buffer.extend_selection(0,2);for cell in buffer.screen().cells.take(2){assert_eq!(format!("{:?}",original(&cell,&ts)),format!("{:?}",cache.line(&cell,&ts,0xfafafa)));}
 cache.prepare(font("JetBrains Mono"),px(18.));assert_eq!(cache.stats(),(0,0));cache.prepare(font("JetBrains Mono"),px(14.));let cell=buffer.screen().cells.next().unwrap();black_box(cache.line(&cell,&ts,0xfafafa));assert!(cache.stats().0>0);
 cache.clear();assert_eq!(cache.stats(),(0,0));println!("Eviction churn, complete long combining sequence and explicit release passed; ShapedLineBytes={}",std::mem::size_of::<ShapedLine>());let timer=cx.background_executor().timer(std::time::Duration::from_millis(25));
 cx.spawn(async move |cx|{timer.await;cx.update(|cx|cx.quit());}).detach();
});
}
