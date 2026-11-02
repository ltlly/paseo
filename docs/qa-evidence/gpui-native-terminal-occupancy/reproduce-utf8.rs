use alacritty_terminal::vte::ansi::{Handler, Processor};
#[derive(Default)]
struct Text(String);
impl Handler for Text {
    fn input(&mut self, c: char) {
        self.0.push(c);
    }
}
fn main() {
    let mut parser: Processor = Processor::new();
    let mut text = Text::default();
    for chunk in [&b"e\xcc"[..], &b"\x81 \xe4"[..], &b"\xb8\xad"[..]] {
        parser.advance(&mut text, chunk);
    }
    println!("expected {:?}, actual {:?}", "e\u{301} 中", text.0);
    assert_eq!(text.0, "e\u{301}中");
}
