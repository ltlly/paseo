use alacritty_terminal::{
    event::{Event, EventListener},
    term::{Config, Term},
    vte::ansi::Processor,
};
use paseo_desktop::terminal::{GridSize, TerminalBuffer};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::Relaxed},
    time::Instant,
};
struct Count;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Relaxed);
        if ENABLED.load(Relaxed) {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_add(n as isize - l.size() as isize, Relaxed);
        if ENABLED.load(Relaxed) {
            CALLS.fetch_add(1, Relaxed);
            BYTES.fetch_add(n, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOC: Count = Count;
struct Silent;
impl EventListener for Silent {
    fn send_event(&self, _: Event) {}
}
fn measure(mut run: impl FnMut()) -> serde_json::Value {
    CALLS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    let live = LIVE.load(Relaxed);
    ENABLED.store(true, Relaxed);
    let t = Instant::now();
    run();
    let elapsed = t.elapsed();
    let live_delta = LIVE.load(Relaxed) - live;
    ENABLED.store(false, Relaxed);
    serde_json::json!({"micros":elapsed.as_micros(),"retainedBytesDelta":live_delta,"allocations":CALLS.load(Relaxed),"allocatedBytes":BYTES.load(Relaxed)})
}
fn main() {
    let size = GridSize {
        rows: 40,
        cols: 160,
    };
    let ascii = ("paseo compile output ".repeat(7) + "\r\n").repeat(15000);
    let tui = ("\x1b[H".to_owned()
        + &(0..40)
            .map(|r| {
                format!(
                    "\x1b[{};1H\x1b[32mrow {r} \x1b[0m{}",
                    r + 1,
                    "x".repeat(140)
                )
            })
            .collect::<String>())
        .repeat(200);
    let unicode =
        ("\x1b[34m中文 cafe\u{301} 👩\u{200d}💻 한글 test \x1b[0m".repeat(4) + "\r\n").repeat(8000);
    for (name, text) in [
        ("ascii_scrollback", ascii),
        ("tui_refresh", tui),
        ("unicode", unicode),
    ] {
        for chunk in [256, 8192] {
            for iteration in 0..3 {
                let mut buffer = TerminalBuffer::new(size, 10000);
                let native = measure(|| {
                    for data in text.as_bytes().chunks(chunk) {
                        buffer.feed(black_box(data));
                    }
                });
                let copied = paseo_desktop::terminal_copy::text(
                    &buffer,
                    -(buffer.history_size() as i32),
                    0,
                    i32::from(size.rows) - 1,
                    usize::from(size.cols) - 1,
                    false,
                    false,
                );
                let mut checksum = 0u64;
                for b in copied.bytes() {
                    checksum = checksum.wrapping_mul(31).wrapping_add(u64::from(b));
                }
                black_box(buffer.screen());
                let mut parser: Processor = Processor::new();
                let mut term = Term::new(
                    Config {
                        scrolling_history: 10000,
                        ..Default::default()
                    },
                    &size,
                    Silent,
                );
                let bare = measure(|| {
                    for data in text.as_bytes().chunks(chunk) {
                        parser.advance(&mut term, black_box(data));
                    }
                });
                black_box(term.grid());
                println!(
                    "{}",
                    serde_json::json!({"workload":name,"bytes":text.len(),"chunk":chunk,"iteration":iteration,"native":native,"bareTerm":bare,"copyChecksum":checksum})
                );
            }
        }
    }
}
