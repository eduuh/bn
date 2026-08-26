use super::*;

pub(crate) fn schedule_text(
    now: i64,
    timer: Option<&timers::RegisteredTimer>,
    wintask: Option<&winsched::WinTask>,
    launchd_registered: bool,
    launchd_loaded: bool,
) -> String {
    if let Some(t) = timer {
        match t.next_epoch {
            Some(next) => format!("systemd · next {}", until(next - now)),
            None => "systemd · scheduled".to_string(),
        }
    } else if let Some(w) = wintask {
        if w.disabled() {
            "windows · DISABLED".to_string()
        } else {
            let base = match w.next_epoch {
                Some(next) => format!("windows · next {}", until(next - now)),
                None => "windows · scheduled".to_string(),
            };
            if w.last_failed() {
                format!("{base} · last FAIL ({})", w.last_result.unwrap_or(-1))
            } else {
                base
            }
        }
    } else if launchd_registered && launchd_loaded {
        "launchd · scheduled".to_string()
    } else {
        "not scheduled".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_loaded_jobs_render_as_scheduled() {
        assert_eq!(schedule_text(0, None, None, true, true), "launchd · scheduled");
    }

    #[test]
    fn launchd_missing_jobs_stay_unscheduled() {
        assert_eq!(schedule_text(0, None, None, false, false), "not scheduled");
    }
}
