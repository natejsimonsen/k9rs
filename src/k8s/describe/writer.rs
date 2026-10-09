//! kubectl's `PrefixWriter` on top of a port of Go's `text/tabwriter`
//! configured like kubectl's `tabbedString` (minwidth 0, tabwidth 8,
//! padding 2, pad char ' ', no flags).

/// Indentation levels (two spaces each), like kubectl's `LEVEL_n`.
pub(crate) const L0: usize = 0;
pub(crate) const L1: usize = 1;
pub(crate) const L2: usize = 2;
pub(crate) const L3: usize = 3;
pub(crate) const L4: usize = 4;

/// Collects describer output. Text written since the last flush is aligned
/// by the tabwriter when `flush` (or `finish`) is called.
pub(crate) struct PrefixWriter {
    pending: String,
    out: String,
    /// Extra levels added by a nested writer (kubectl `NewNestedPrefixWriter`).
    indent: usize,
}

impl PrefixWriter {
    pub(crate) fn new() -> Self {
        Self {
            pending: String::new(),
            out: String::new(),
            indent: 0,
        }
    }

    /// `Write(level, ...)`: prefix with 2 spaces per level, escape terminal chars.
    pub(crate) fn write(&mut self, level: usize, text: &str) {
        let level = level + self.indent;
        for _ in 0..level {
            self.pending.push_str("  ");
        }
        escape_into(&mut self.pending, text);
    }

    /// `WriteLine(s)`: the text plus a newline. A nested writer indents it.
    pub(crate) fn write_line(&mut self, text: &str) {
        if self.indent > 0 {
            let line = format!("{text}\n");
            self.write(0, &line);
        } else {
            escape_into(&mut self.pending, text);
            self.pending.push('\n');
        }
    }

    /// Run `f` with a writer nested `levels` deeper.
    pub(crate) fn nested(&mut self, levels: usize, f: impl FnOnce(&mut PrefixWriter)) {
        self.indent += levels;
        f(self);
        self.indent -= levels;
    }

    /// Flush the tabwriter: align everything written so far.
    pub(crate) fn flush(&mut self) {
        let text = std::mem::take(&mut self.pending);
        self.out.push_str(&tabwrite(&text));
    }

    pub(crate) fn finish(mut self) -> String {
        self.flush();
        self.out
    }
}

/// printers.WriteEscaped: replace ESC and CR so output can't drive the terminal.
fn escape_into(dst: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\x1b' => dst.push_str("^["),
            '\r' => dst.push_str("\\r"),
            _ => dst.push(c),
        }
    }
}

struct Cell {
    text: String,
    width: usize,
}

const PADDING: usize = 2;

/// Format `text` exactly like a Go tabwriter.Writer(0, 8, 2, ' ', 0) that is
/// written `text` and then flushed.
pub(crate) fn tabwrite(text: &str) -> String {
    let mut lines: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '\t' | '\x0b' | '\n' | '\x0c' => {
                let width = cur.chars().count();
                let cell = Cell {
                    text: std::mem::take(&mut cur),
                    width,
                };
                lines.last_mut().expect("line").push(cell);
                if ch == '\n' || ch == '\x0c' {
                    lines.push(Vec::new());
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        let width = cur.chars().count();
        lines
            .last_mut()
            .expect("line")
            .push(Cell { text: cur, width });
    }
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut widths = Vec::new();
    format_block(&lines, &mut widths, 0, lines.len(), &mut out);
    out
}

fn format_block(
    lines: &[Vec<Cell>],
    widths: &mut Vec<usize>,
    line0: usize,
    line1: usize,
    out: &mut String,
) {
    let column = widths.len();
    let mut line0 = line0;
    let mut this = line0;
    while this < line1 {
        if column as isize >= lines[this].len() as isize - 1 {
            this += 1;
            continue;
        }
        write_lines(lines, widths, line0, this, out);
        line0 = this;
        let mut width = 0;
        while this < line1 {
            let line = &lines[this];
            if column as isize >= line.len() as isize - 1 {
                break;
            }
            let c = &line[column];
            width = width.max(c.width + PADDING);
            this += 1;
        }
        widths.push(width);
        format_block(lines, widths, line0, this, out);
        widths.pop();
        line0 = this;
    }
    write_lines(lines, widths, line0, line1, out);
}

fn write_lines(
    lines: &[Vec<Cell>],
    widths: &[usize],
    line0: usize,
    line1: usize,
    out: &mut String,
) {
    for (i, line) in lines.iter().enumerate().take(line1).skip(line0) {
        for (j, c) in line.iter().enumerate() {
            out.push_str(&c.text);
            if j < widths.len() {
                for _ in c.width..widths[j] {
                    out.push(' ');
                }
            }
        }
        if i + 1 != lines.len() {
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns_in_blocks() {
        let got = tabwrite("Name:\tfoo\nNamespace:\tbar\nplain\nA:\tb\n");
        assert_eq!(got, "Name:       foo\nNamespace:  bar\nplain\nA:  b\n");
    }

    #[test]
    fn nested_columns() {
        let got = tabwrite("  Type\tStatus\n  Ready \tTrue \n");
        assert_eq!(got, "  Type    Status\n  Ready   True \n");
    }

    #[test]
    fn trailing_text_without_newline() {
        assert_eq!(tabwrite("a\tb"), "a  b");
        assert_eq!(tabwrite(""), "");
    }

    #[test]
    fn empty_cells_pad() {
        let got = tabwrite("a\t\tc\nbbbb\tx\ty\n");
        assert_eq!(got, "a        c\nbbbb  x  y\n");
    }

    #[test]
    fn unicode_width_is_rune_count() {
        assert_eq!(tabwrite("é\tx\nab\ty\n"), "é   x\nab  y\n");
    }

    #[test]
    fn prefix_writer_levels_and_escape() {
        let mut w = PrefixWriter::new();
        w.write(L1, "Key:\tv\x1b\r\n");
        w.nested(L2, |w| {
            w.write(L0, "Labels:\t");
            w.write_line("<none>");
        });
        assert_eq!(w.finish(), "  Key:       v^[\\r\n    Labels:      <none>\n");
    }
}
