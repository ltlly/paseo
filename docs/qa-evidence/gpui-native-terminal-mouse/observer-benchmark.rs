use std::{alloc::{GlobalAlloc,Layout,System},sync::atomic::{AtomicBool,AtomicUsize,Ordering::Relaxed},hint::black_box,time::Instant};
use paseo_desktop::terminal::{TerminalBuffer,GridSize};
struct Count;static ENABLED:AtomicBool=AtomicBool::new(false);static CALLS:AtomicUsize=AtomicUsize::new(0);static BYTES:AtomicUsize=AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count{unsafe fn alloc(&self,l:Layout)->*mut u8{if ENABLED.load(Relaxed){CALLS.fetch_add(1,Relaxed);BYTES.fetch_add(l.size(),Relaxed);}unsafe{System.alloc(l)}}unsafe fn dealloc(&self,p:*mut u8,l:Layout){unsafe{System.dealloc(p,l)}}unsafe fn realloc(&self,p:*mut u8,l:Layout,n:usize)->*mut u8{if ENABLED.load(Relaxed){CALLS.fetch_add(1,Relaxed);BYTES.fetch_add(n,Relaxed);}unsafe{System.realloc(p,l,n)}}}
#[global_allocator] static ALLOC:Count=Count;
fn main(){
 for (name,data) in [("plain",b"x".repeat(8*1024*1024)),("osc",[b"\x1b]52;c;".as_slice(),&b"a".repeat(8*1024*1024),b"\x07\x1b[?1003;1016h"].concat()),("dcs",[b"\x1bPq".as_slice(),&b"a".repeat(8*1024*1024),b"\x1b\\\x1b[?1003;1016h"].concat()),("ansi",b"\x1b[31mhello\x1b[0m\x1b[?1003;1016h".repeat(100_000))]{
  let mut tracker=paseo_desktop::mouse::ModeTracker::default();tracker.feed(&data);
  CALLS.store(0,Relaxed);BYTES.store(0,Relaxed);ENABLED.store(true,Relaxed);let start=Instant::now();
  for _ in 0..100{tracker.feed(black_box(&data));black_box(tracker.modes());}
  let ns=start.elapsed().as_nanos();ENABLED.store(false,Relaxed);
  println!("name={name} bytes={} iterations=100 nsPerIteration={} allocations={} allocatedBytes={} stateBytes={} modes={:?}",data.len(),ns/100,CALLS.load(Relaxed),BYTES.load(Relaxed),std::mem::size_of_val(&tracker),tracker.modes());assert_eq!(CALLS.load(Relaxed),0);
 }
}
