//! Compact ANSI progress view for `day` runs — no TUI framework, deterministic
//! pure-render core (testable) plus a thin terminal writer.
use std::io::Write;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneVisual {
    Queued,
    Running,
    Passed,
    Failed,
    TimedOut,
    Canceled,
    InfraError,
}

#[derive(Debug, Clone)]
pub struct LaneFrame {
    pub agent_id: String,
    pub branch: String,
    pub visual: LaneVisual,
    pub elapsed_ms: u64,
    pub note: Option<String>,
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn lane_glyph(visual: &LaneVisual, tick: usize) -> (char, &'static str) {
    match visual {
        LaneVisual::Queued => ('·', "\x1b[90m"),
        LaneVisual::Running => (SPINNER[tick % SPINNER.len()], "\x1b[36m"),
        LaneVisual::Passed => ('✓', "\x1b[32m"),
        LaneVisual::Failed => ('✗', "\x1b[31m"),
        LaneVisual::TimedOut => ('⏱', "\x1b[33m"),
        LaneVisual::Canceled => ('⊘', "\x1b[33m"),
        LaneVisual::InfraError => ('!', "\x1b[35m"),
    }
}

/// Pure frame renderer (no terminal IO): goal, one row per lane, footer.
pub fn render_frame(goal: &str, lanes: &[LaneFrame], tick: usize, elapsed_ms: u64) -> String {
    let mut out = String::with_capacity(256 + lanes.len() * 96);
    out.push_str("\x1b[1mleio-harness day\x1b[0m  ");
    out.push_str(goal);
    out.push('\n');
    for lane in lanes {
        let (glyph, color) = lane_glyph(&lane.visual, tick);
        let branch = truncate(&lane.branch, 48);
        out.push_str(&format!(
            "{color}{glyph}\x1b[0m {:<18} {:<48} {:>7}\n",
            lane.agent_id,
            branch,
            format_ms(lane.elapsed_ms)
        ));
        if let Some(note) = &lane.note {
            out.push_str(&format!("  \x1b[90m└ {}\x1b[0m\n", truncate(note, 72)));
        }
    }
    let done = lanes
        .iter()
        .filter(|lane| !matches!(lane.visual, LaneVisual::Queued | LaneVisual::Running))
        .count();
    out.push_str(&format!(
        "\x1b[90m{done}/{} lanes · {}\x1b[0m",
        lanes.len(),
        format_ms(elapsed_ms)
    ));
    out
}

pub fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn format_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

/// Live terminal writer: full-frame redraw with cursor-up, deterministic
/// ordering (goal rows fixed, spinner animated by tick).
pub struct TerminalView {
    lines: usize,
    tick: usize,
    started: Instant,
}

impl Default for TerminalView {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalView {
    pub fn new() -> Self {
        let mut stderr = std::io::stderr();
        let _ = stderr.write_all(b"\x1b[?25l"); // hide cursor
        Self {
            lines: 0,
            tick: 0,
            started: Instant::now(),
        }
    }

    pub fn draw(&mut self, goal: &str, lanes: &[LaneFrame]) {
        let frame = render_frame(
            goal,
            lanes,
            self.tick,
            self.started.elapsed().as_millis() as u64,
        );
        let mut stderr = std::io::stderr();
        if self.lines > 0 {
            let _ = write!(stderr, "\x1b[{}A", self.lines);
        }
        let _ = write!(stderr, "{frame}");
        let new_lines = frame.matches('\n').count();
        // Clear leftovers from the previous frame.
        for _ in new_lines..self.lines {
            let _ = writeln!(stderr, "\x1b[2K");
        }
        let _ = stderr.flush();
        self.lines = new_lines;
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn finish(&mut self, goal: &str, lanes: &[LaneFrame]) {
        self.draw(goal, lanes);
        let mut stderr = std::io::stderr();
        let _ = stderr.write_all(b"\n\x1b[?25h\n"); // newline + show cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_deterministic_per_tick() {
        let lanes = vec![
            LaneFrame {
                agent_id: "codex".to_owned(),
                branch: "agents/codex/run-1".to_owned(),
                visual: LaneVisual::Passed,
                elapsed_ms: 1_250,
                note: None,
            },
            LaneFrame {
                agent_id: "kimi".to_owned(),
                branch: "agents/kimi/run-2".to_owned(),
                visual: LaneVisual::Running,
                elapsed_ms: 62_000,
                note: Some("compiling".to_owned()),
            },
        ];
        let first = render_frame("goal", &lanes, 3, 5_000);
        let second = render_frame("goal", &lanes, 3, 5_000);
        assert_eq!(first, second);
        assert!(first.contains("codex"));
        assert!(first.contains("✓"));
        assert!(first.contains("1/2 lanes"));
        assert!(first.contains("1m02s"));
        assert!(first.contains("1.2s"));
    }

    #[test]
    fn truncate_bounds_length() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
