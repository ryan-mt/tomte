use super::super::{context_gauge, status_left_text_for_parts};
use crate::tui::palette;

#[test]
fn context_gauge_hidden_before_any_usage() {
    assert!(context_gauge(0, 1_000_000).is_none());
}

#[test]
fn context_gauge_ramps_calm_warning_danger() {
    assert_eq!(
        context_gauge(500_000, 1_000_000).unwrap(),
        ("50% ctx".to_string(), palette::TEXT_MUTED)
    );
    assert_eq!(
        context_gauge(700_000, 1_000_000).unwrap(),
        ("70% ctx".to_string(), palette::WARNING)
    );
    assert_eq!(
        context_gauge(900_000, 1_000_000).unwrap(),
        ("90% ctx".to_string(), palette::DANGER)
    );
}

#[test]
fn context_gauge_caps_at_100_and_survives_zero_limit() {
    assert_eq!(context_gauge(2_000_000, 1_000_000).unwrap().0, "100% ctx");
    assert_eq!(context_gauge(5, 0).unwrap().0, "100% ctx");
}

#[test]
fn includes_goal_elapsed_when_goal_is_active() {
    assert_eq!(
        status_left_text_for_parts("default", "", false, Some("1m32")),
        "default  ·  goal 1m32  ·  shift+tab cycles mode · ? for shortcuts"
    );
}

#[test]
fn keeps_status_activity_after_goal_elapsed() {
    assert_eq!(
        status_left_text_for_parts("plan", "(continuing active goal...)", false, Some("12s")),
        "plan  ·  goal 12s  ·  (continuing active goal...)"
    );
}

// The armed quit guard must be visible: without the hint, a first Ctrl+C
// looks like the key was ignored.
#[test]
fn status_left_text_appends_quit_hint_while_armed() {
    use super::super::status_left_text;
    use crate::tui::app::App;
    let mut app = App::new();
    assert!(!status_left_text(&app).contains("ctrl+c again to quit"));
    app.ctrl_c_armed_at = Some(std::time::Instant::now());
    assert!(status_left_text(&app).contains("ctrl+c again to quit"));
}
