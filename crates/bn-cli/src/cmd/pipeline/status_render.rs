use bn_core::pipeline::status::{PhaseStatus, PipelineView};

pub(super) fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width >= 20)
        .unwrap_or(100)
}

pub(super) fn render_pipeline(view: &PipelineView, width: usize) -> String {
    if width < 72 {
        render_narrow(view, width)
    } else {
        render_wide(view)
    }
}

fn render_wide(view: &PipelineView) -> String {
    let tracker = view
        .phases
        .iter()
        .map(|phase| format!("{} {}", status_glyph(phase.status), phase.id.label()))
        .collect::<Vec<_>>()
        .join(" ───→ ");
    let mut out = format!(
        "{}/{}\n{tracker}\n▶ {}",
        view.repo, view.branch, view.action
    );
    if let Some(goal) = &view.goal {
        out.push_str(&format!("\n  {goal}"));
    }
    out
}

fn render_narrow(view: &PipelineView, width: usize) -> String {
    let mut lines = vec![format!("{}/{}", view.repo, view.branch)];
    lines.extend(
        view.phases
            .iter()
            .map(|phase| format!("{} {}", status_glyph(phase.status), phase.id.label())),
    );
    lines.extend(wrap_line("▶ ", &view.action, width));
    if let Some(goal) = &view.goal {
        lines.extend(wrap_line("  ", goal, width));
    }
    lines.join("\n")
}

pub(super) fn render_pipeline_list(views: &[PipelineView]) -> String {
    if views.is_empty() {
        return "No active tasks.".to_string();
    }
    views
        .iter()
        .map(|view| {
            format!(
                "{} {:<9} {}/{} — {}",
                status_glyph(PhaseStatus::Active),
                view.phase.label(),
                view.repo,
                view.branch,
                view.action
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_line(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let continuation = " ".repeat(prefix.chars().count());
    let mut lines = Vec::new();
    let mut current = prefix.to_string();
    for word in text.split_whitespace() {
        let separator = usize::from(current.chars().count() > prefix.chars().count());
        if current.chars().count() + separator + word.chars().count() > width
            && current.chars().count() > prefix.chars().count()
        {
            lines.push(current);
            current = continuation.clone();
        }
        if current.chars().count() > prefix.chars().count() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if current.chars().count() > prefix.chars().count() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn status_glyph(status: PhaseStatus) -> &'static str {
    match status {
        PhaseStatus::Done => "✓",
        PhaseStatus::Active => "▶",
        PhaseStatus::Pending => "·",
    }
}
