use paseo_desktop::terminal::{GridSize, TerminalBuffer};
fn main() {
    let directory =
        std::path::PathBuf::from(std::env::args().nth(1).expect("output directory required"));
    std::fs::create_dir_all(&directory).unwrap();
    let text =
        ("\x1b[34m中文 cafe\u{301} 👩\u{200d}💻 한글 test \x1b[0m".repeat(4) + "\r\n").repeat(8000);
    for chunk in [1, 7, 256, 8192, text.len()] {
        let mut buffer = TerminalBuffer::new(
            GridSize {
                rows: 40,
                cols: 160,
            },
            10000,
        );
        for data in text.as_bytes().chunks(chunk) {
            buffer.feed(data);
        }
        let copy = paseo_desktop::terminal_copy::text(
            &buffer,
            -(buffer.history_size() as i32),
            0,
            39,
            159,
            false,
            false,
        );
        let mut grid = String::new();
        for line in -(buffer.history_size() as i32)..40 {
            buffer.push_row_text(line, 0, 160, &mut grid);
            grid.push('\n');
        }
        std::fs::write(directory.join(format!("copy-{chunk}.txt")), &copy).unwrap();
        std::fs::write(directory.join(format!("grid-{chunk}.txt")), &grid).unwrap();
        println!(
            "chunk {chunk} history {} rows {} copy {} grid {}",
            buffer.history_size(),
            buffer.written_rows(),
            copy.len(),
            grid.len()
        );
    }
}
