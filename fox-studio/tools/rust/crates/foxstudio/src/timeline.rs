//! Run timeline: per-stage start / end / result parsed from foxbuild's output (the live run, or the last run in
//! build.log), drawn as a Gantt chart. Pure parsing is tested; the chart is a painter widget.
use crate::build_events::{BuildEvent, KnownEventKind};
use crate::build_view::Live;
use crate::theme::Palette;
use eframe::egui::{self, RichText, Sense, Stroke, Vec2};

/// one stage in a run
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub name: String,
    /// seconds after the run started
    pub start: f32,
    pub end: Option<f32>,
    pub state: Live,
    pub peak_mb: Option<f32>,
    /// "PYTHON FALLBACK: ..." lines for this stage
    pub fallbacks: Vec<String>,
    /// the FAILED line's reason
    pub failure: Option<String>,
    /// finished with byte-identical outputs (early cutoff)
    pub unchanged: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    /// the header line ("=== foxbuild ...")
    pub header: Option<String>,
    pub spans: Vec<Span>,
    /// last timestamp seen (seconds)
    pub last_t: f32,
    /// "wall N s, stage time M s ..." line
    pub summary: Option<String>,
    /// stages reported blocked by an upstream failure
    pub blocked: Vec<String>,
    pub dry_run: bool,
}

/// "[  1:02] rest" / "[1:02:03] rest" -> (seconds, rest)
pub fn stamp(line: &str) -> Option<(f32, &str)> {
    let l = line.strip_prefix('[')?;
    let close = l.find(']')?;
    let t = l[..close].trim();
    let mut secs = 0f32;
    for part in t.split(':') {
        secs = secs * 60.0 + part.trim().parse::<f32>().ok()?;
    }
    Some((secs, l[close + 1..].trim_start()))
}

/// Parse foxbuild output. With several runs in the text (build.log), only the last one is kept.
pub fn parse<'a>(lines: impl IntoIterator<Item = &'a str>) -> Timeline {
    let mut tl = Timeline::default();
    let mut in_timing = false;
    for line in lines {
        let Some((t, rest)) = stamp(line) else {
            continue;
        };
        tl.last_t = tl.last_t.max(t);
        if rest.starts_with("=== foxbuild") {
            tl = Timeline {
                header: Some(rest.to_string()),
                dry_run: rest.contains("DRY RUN"),
                last_t: t,
                ..Default::default()
            };
            in_timing = false;
            continue;
        }
        if rest.starts_with("--- timing") {
            in_timing = true;
            continue;
        }
        if rest.starts_with("wall ") {
            tl.summary = Some(rest.to_string());
            continue;
        }
        if in_timing {
            if rest.contains("blocked (an upstream stage failed)")
                && let Some(n) = rest.split_whitespace().next()
            {
                tl.blocked.push(n.to_string());
            }
            continue;
        }
        let mut it = rest.split_whitespace();
        let verb = it.next().unwrap_or("");
        let name = it.next().unwrap_or("").trim_end_matches(':').to_string();
        match verb {
            "start" if !name.is_empty() => {
                tl.spans.retain(|s| s.name != name);
                tl.spans.push(Span {
                    name,
                    start: t,
                    end: None,
                    state: Live::Running,
                    peak_mb: None,
                    fallbacks: vec![],
                    failure: None,
                    unchanged: false,
                });
            }
            "done" => {
                if let Some(s) = tl.spans.iter_mut().rev().find(|s| s.name == name) {
                    s.end = Some(t);
                    s.state = Live::Done;
                    s.unchanged = rest.contains("(unchanged: early cutoff)");
                    // "done   name   12 s   900 MB peak ..."
                    let w: Vec<&str> = rest.split_whitespace().collect();
                    if let Some(i) = w.iter().position(|x| *x == "MB") {
                        s.peak_mb = w.get(i.wrapping_sub(1)).and_then(|v| v.parse().ok());
                    }
                }
            }
            "FAILED" => {
                let reason = rest
                    .splitn(3, char::is_whitespace)
                    .nth(2)
                    .map(|r| r.trim().to_string());
                match tl.spans.iter_mut().rev().find(|s| s.name == name) {
                    Some(s) => {
                        s.end = Some(t);
                        s.state = Live::Failed;
                        s.failure = reason;
                    }
                    None => tl.spans.push(Span {
                        name,
                        start: t,
                        end: Some(t),
                        state: Live::Failed,
                        peak_mb: None,
                        fallbacks: vec![],
                        failure: reason,
                        unchanged: false,
                    }),
                }
            }
            _ => {
                // "       nav.ground: PYTHON FALLBACK: tools/x.py kernel y"
                if let Some(i) = rest.find(": PYTHON FALLBACK: ") {
                    let n = rest[..i].trim();
                    if let Some(s) = tl.spans.iter_mut().rev().find(|s| s.name == n) {
                        s.fallbacks.push(rest[i + 19..].trim().to_string());
                    }
                }
            }
        }
    }
    tl
}

impl Timeline {
    /// Typed session events; the legacy parser is used only for retained logs.
    pub fn apply_event(&mut self, event: &BuildEvent) {
        let time = (event.elapsed_ms as f64 / 1000.0) as f32;
        self.last_t = self.last_t.max(time);
        if event.kind == Some(KnownEventKind::RunStarted) {
            let operation = event
                .data
                .get("operation")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("build");
            *self = Self {
                header: Some(format!("{operation} · {}", event.run_id)),
                dry_run: operation == "dry_run",
                last_t: time,
                ..Default::default()
            };
            return;
        }
        if event.kind == Some(KnownEventKind::RunFinished) {
            self.summary =
                Some("Build reported completion; checking child exit and output EOF.".into());
            return;
        }
        if !matches!(
            event.kind,
            Some(KnownEventKind::StageStarted | KnownEventKind::StageFinished)
        ) {
            return;
        }
        let Some(name) = event.data.get("name").and_then(serde_json::Value::as_str) else {
            return;
        };
        // Keep GUI history bounded independently of stream length or log retention.
        if name.len() > 256 || (self.spans.len() >= 4096 && self.span(name).is_none()) {
            return;
        }
        let Some((state, _)) = crate::build_view::live_event(event) else {
            return;
        };
        if state == Live::Blocked && !self.blocked.iter().any(|stage| stage == name) {
            self.blocked.push(name.to_owned());
        }
        if event.kind == Some(KnownEventKind::StageStarted) {
            self.spans.retain(|span| span.name != name);
            self.spans.push(Span {
                name: name.to_owned(),
                start: time,
                end: None,
                state,
                peak_mb: None,
                fallbacks: vec![],
                failure: None,
                unchanged: false,
            });
            return;
        }
        let seconds = event
            .data
            .get("seconds")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;
        let index = match self.spans.iter().position(|span| span.name == name) {
            Some(index) => index,
            None => {
                self.spans.push(Span {
                    name: name.to_owned(),
                    start: (time - seconds).max(0.0),
                    end: None,
                    state,
                    peak_mb: None,
                    fallbacks: vec![],
                    failure: None,
                    unchanged: false,
                });
                self.spans.len() - 1
            }
        };
        let span = &mut self.spans[index];
        span.end = Some(time);
        span.state = state;
        span.peak_mb = event
            .data
            .get("peak_mb")
            .and_then(serde_json::Value::as_f64)
            .map(|value| value.min(f32::MAX.into()) as f32);
        span.failure = event
            .data
            .get("error")
            .or_else(|| event.data.get("reason"))
            .and_then(serde_json::Value::as_str)
            .map(|text| text.chars().take(2048).collect());
        span.fallbacks = event
            .data
            .get("fallbacks")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .take(64)
            .map(|text| text.chars().take(512).collect())
            .collect();
        span.unchanged = event
            .data
            .get("unchanged")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    }
    pub fn failed(&self) -> Vec<&Span> {
        self.spans
            .iter()
            .filter(|s| s.state == Live::Failed)
            .collect()
    }
    pub fn with_fallbacks(&self) -> Vec<&Span> {
        self.spans
            .iter()
            .filter(|s| !s.fallbacks.is_empty())
            .collect()
    }
    pub fn span(&self, name: &str) -> Option<&Span> {
        self.spans.iter().rev().find(|s| s.name == name)
    }
}

/// Gantt chart. `now` = seconds since the run started while it runs. Returns a clicked stage.
pub fn chart(
    ui: &mut egui::Ui,
    tl: &Timeline,
    now: Option<f32>,
    selected: Option<&str>,
    p: &Palette,
) -> Option<String> {
    if tl.spans.is_empty() {
        ui.label(RichText::new(if tl.header.is_some() { "No stage ran in this run (everything was up to date)." } else { "No run to show yet: start a build, or one runs from a terminal and shows here from build.log." }).color(p.muted));
        return None;
    }
    let mut spans: Vec<&Span> = tl.spans.iter().collect();
    spans.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.name.cmp(&b.name)));
    let t_end = spans
        .iter()
        .map(|s| s.end.unwrap_or(now.unwrap_or(tl.last_t)))
        .fold(tl.last_t, f32::max)
        .max(1.0);
    let label_w = 210.0;
    let row_h = 20.0;
    let mut clicked = None;
    egui::ScrollArea::both()
        .id_salt("timeline_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = (ui.available_width() - 8.0).max(label_w + 200.0);
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(width, row_h * (spans.len() as f32 + 1.5)),
                Sense::hover(),
            );
            let painter = ui.painter_at(rect);
            let x0 = rect.left() + label_w;
            let scale = (rect.right() - 12.0 - x0) / t_end;
            // time axis
            let step = nice_step(t_end / 8.0);
            let mut t = 0.0;
            while t <= t_end + 0.01 {
                let x = x0 + t * scale;
                painter.line_segment(
                    [
                        egui::pos2(x, rect.top() + row_h),
                        egui::pos2(x, rect.bottom()),
                    ],
                    Stroke::new(1.0, p.muted.gamma_multiply(0.2)),
                );
                painter.text(
                    egui::pos2(x, rect.top() + 2.0),
                    egui::Align2::CENTER_TOP,
                    fmt_t(t),
                    egui::FontId::proportional(10.5),
                    p.muted,
                );
                t += step;
            }
            for (i, s) in spans.iter().enumerate() {
                let y = rect.top() + row_h * (i as f32 + 1.2);
                let row = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    Vec2::new(rect.width(), row_h),
                );
                let resp = ui.interact(row, ui.id().with(("tl", i)), Sense::click());
                if selected == Some(s.name.as_str()) {
                    painter.rect_filled(row, 3, p.accent.gamma_multiply(0.15));
                } else if resp.hovered() {
                    painter.rect_filled(row, 3, p.muted.gamma_multiply(0.08));
                }
                let color = match s.state {
                    Live::Failed => p.err,
                    Live::Cancelled | Live::Blocked => p.warn,
                    Live::Running => p.info,
                    _ if !s.fallbacks.is_empty() => p.warn,
                    _ if s.unchanged => p.ok.gamma_multiply(0.6),
                    _ => p.ok,
                };
                painter.text(
                    egui::pos2(rect.left() + 4.0, y + row_h / 2.0),
                    egui::Align2::LEFT_CENTER,
                    &s.name,
                    egui::FontId::monospace(12.0),
                    ui.visuals().text_color(),
                );
                let end = s.end.unwrap_or(now.unwrap_or(tl.last_t));
                let bar = egui::Rect::from_min_max(
                    egui::pos2(x0 + s.start * scale, y + 3.0),
                    egui::pos2(
                        (x0 + end * scale).max(x0 + s.start * scale + 3.0),
                        y + row_h - 3.0,
                    ),
                );
                painter.rect_filled(
                    bar,
                    3,
                    color.gamma_multiply(if s.end.is_none() { 0.6 } else { 0.85 }),
                );
                let mut txt = fmt_t(end - s.start);
                if !s.fallbacks.is_empty() {
                    txt.push_str("  PY");
                }
                painter.text(
                    egui::pos2(bar.right() + 4.0, y + row_h / 2.0),
                    egui::Align2::LEFT_CENTER,
                    txt,
                    egui::FontId::proportional(10.5),
                    p.muted,
                );
                let resp = resp.on_hover_ui(|ui| {
                    ui.label(RichText::new(&s.name).monospace().strong());
                    ui.label(format!(
                        "{} → {}  ({})",
                        fmt_t(s.start),
                        s.end.map(fmt_t).unwrap_or_else(|| "running".into()),
                        fmt_t(end - s.start)
                    ));
                    if let Some(m) = s.peak_mb {
                        ui.label(format!("peak {m:.0} MB"));
                    }
                    if s.unchanged {
                        ui.label("outputs unchanged (early cutoff)");
                    }
                    if let Some(f) = &s.failure {
                        ui.colored_label(p.err, f);
                    }
                    for f in &s.fallbacks {
                        ui.colored_label(p.warn, format!("Python fallback: {f}"));
                    }
                });
                if resp.clicked() {
                    clicked = Some(s.name.clone());
                }
            }
            if let Some(n) = now {
                let x = x0 + n * scale;
                painter.line_segment(
                    [
                        egui::pos2(x, rect.top() + row_h),
                        egui::pos2(x, rect.bottom()),
                    ],
                    Stroke::new(1.5, p.info),
                );
            }
        });
    clicked
}

fn nice_step(raw: f32) -> f32 {
    for s in [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ] {
        if s >= raw {
            return s;
        }
    }
    7200.0
}

pub fn fmt_t(s: f32) -> String {
    let s = s.max(0.0);
    if s >= 3600.0 {
        format!("{}h{:02}m", (s / 3600.0) as u32, ((s / 60.0) as u32) % 60)
    } else if s >= 60.0 {
        format!("{}:{:02}", (s / 60.0) as u32, (s as u32) % 60)
    } else {
        format!("{s:.0} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "[  0:00] === foxbuild 2026-10-05 x (3 stages selected)
[  0:00] start  a.one                          (est 2048 MB; never built)
[  0:01] start  b.two                          (est 1024 MB; input changed: x)
[  0:12] done   a.one                              11 s    900 MB peak  3 outputs (unchanged: early cutoff), 4 inputs (0 static), 2 code files
[  0:12]        a.one: PYTHON FALLBACK: tools/location/x.py kernel nav_rasterize
[  1:05] FAILED b.two                          rc Some(1) after 64 s: see work/build/logs/b.two.log
[  1:05] --- timing ---------------------------------------------------------------
[  1:05] stage                            seconds   peak MB  result
[  1:05] a.one                               11.0       900  ran (PYTHON FALLBACK)
[  1:05] b.two                               64.0         0  FAILED
[  1:05] c.three                                -         -  blocked (an upstream stage failed)
[  1:05] wall 65 s, stage time 75 s (parallel x1.2), FAILURES";

    #[test]
    fn parse_a_run() {
        let tl = parse(RUN.lines());
        assert!(tl.header.as_deref().unwrap().contains("3 stages"));
        assert_eq!(tl.spans.len(), 2);
        let a = tl.span("a.one").unwrap();
        assert_eq!(
            (a.start, a.end, a.state, a.peak_mb, a.unchanged),
            (0.0, Some(12.0), Live::Done, Some(900.0), true)
        );
        assert_eq!(
            a.fallbacks,
            vec!["tools/location/x.py kernel nav_rasterize".to_string()]
        );
        let b = tl.span("b.two").unwrap();
        assert_eq!((b.start, b.end, b.state), (1.0, Some(65.0), Live::Failed));
        assert!(
            b.failure
                .as_deref()
                .unwrap()
                .starts_with("rc Some(1) after 64 s")
        );
        assert_eq!(tl.blocked, vec!["c.three".to_string()]);
        assert!(tl.summary.as_deref().unwrap().contains("FAILURES"));
        assert_eq!(tl.failed().len(), 1);
        assert_eq!(tl.with_fallbacks().len(), 1);
    }

    #[test]
    fn last_run_wins_and_stamps() {
        let two =
            format!("{RUN}\n[  0:00] === foxbuild again DRY RUN\n[  0:00] RUN    a.one   0 s");
        let tl = parse(two.lines());
        assert!(tl.dry_run);
        assert!(tl.spans.is_empty());
        assert_eq!(stamp("[1:02:03] x"), Some((3723.0, "x")));
        assert_eq!(stamp("[  0:07] start"), Some((7.0, "start")));
        assert_eq!(stamp("no stamp"), None);
        assert_eq!(fmt_t(75.0), "1:15");
    }
}
