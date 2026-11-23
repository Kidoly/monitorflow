use super::*;

/// Minute `m` of a fixed day.
fn t(m: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + m * 60, 0).unwrap()
}

fn s(list: &[(i64, i64)]) -> Spans {
    Spans::from_spans(list.iter().map(|&(a, b)| (t(a), t(b))))
}

fn source(down: &[(i64, i64)], cover: &[(i64, i64)], cause: &'static str) -> Source<&'static str> {
    Source {
        down: s(down),
        cover: s(cover),
        cause,
    }
}

const DAY: (i64, i64) = (0, 1440);

fn day() -> Span {
    (t(DAY.0), t(DAY.1))
}

#[test]
fn nothing_down_is_fully_available() {
    let measured = measure::<&str>(day(), &Spans::new(), &Spans::new(), &[]);
    assert_eq!(measured.availability(), Some(1.0));
    assert_eq!((measured.down, measured.maintenance), (0.0, 0.0));
    assert!(measured.outages.is_empty());
}

#[test]
fn outages_merge_overlapping_causes_and_keep_them() {
    let sources = [
        source(&[(100, 130)], &[], "disk"),
        source(&[(120, 160)], &[], "offline"),
        source(&[(600, 610)], &[], "disk"),
    ];
    let measured = measure(day(), &Spans::new(), &Spans::new(), &sources);
    assert_eq!(measured.down, 70.0 * 60.0);
    assert_eq!(measured.outages.len(), 2);
    assert_eq!(measured.outages[0].causes, vec!["disk", "offline"]);
    assert_eq!(
        (measured.outages[0].start, measured.outages[0].end),
        (t(100), t(160))
    );
    assert_eq!(measured.longest(), 60.0 * 60.0);
    let expected = 1.0 - 70.0 / 1440.0;
    assert!((measured.availability().unwrap() - expected).abs() < 1e-12);
}

#[test]
fn planned_maintenance_leaves_the_count() {
    // Down 60 minutes, 40 of them in a window: 20 count, and the window's
    // 120 minutes are out of the 1440.
    let sources = [source(&[(100, 160)], &[], "offline")];
    let measured = measure(day(), &s(&[(140, 260)]), &Spans::new(), &sources);
    assert_eq!(measured.down, 40.0 * 60.0);
    assert_eq!(measured.maintenance, 120.0 * 60.0);
    assert_eq!(measured.counted, 1320.0 * 60.0);
    assert_eq!(measured.outages[0].end, t(140));
}

#[test]
fn a_silence_of_one_check_excuses_only_its_incidents() {
    let sources = [
        source(&[(100, 160)], &[(90, 200)], "postgresql"),
        source(&[(150, 170)], &[], "disk"),
    ];
    let measured = measure(day(), &Spans::new(), &Spans::new(), &sources);
    assert_eq!(measured.down, 20.0 * 60.0);
    assert_eq!(measured.outages[0].causes, vec!["disk"]);
    // A check silence is not maintenance of the whole thing.
    assert_eq!(measured.maintenance, 0.0);
}

#[test]
fn down_time_outside_the_period_is_cut() {
    let sources = [source(&[(-60, 30), (1430, 1500)], &[], "offline")];
    let measured = measure(day(), &Spans::new(), &Spans::new(), &sources);
    assert_eq!(measured.down, 40.0 * 60.0);
    assert_eq!(measured.outages.len(), 2);
}

#[test]
fn all_maintenance_is_not_measured() {
    let measured = measure::<&str>(day(), &s(&[DAY]), &Spans::new(), &[]);
    assert_eq!(measured.availability(), None);
}

fn member(down: &[(i64, i64)], maintenance: &[(i64, i64)]) -> Timeline {
    Timeline {
        down: s(down),
        maintenance: s(maintenance),
        unobserved: Spans::new(),
    }
}

#[test]
fn a_service_of_all_members_is_down_when_any_is() {
    let members = [member(&[(10, 20)], &[]), member(&[(15, 30)], &[])];
    let measured = combine(day(), Rule::All, &members, &Spans::new());
    assert_eq!(measured.down, 20.0 * 60.0);
    assert_eq!(measured.outages.len(), 1);
    assert_eq!(measured.outages[0].causes, vec![0, 1]);
}

#[test]
fn a_service_of_at_least_n_tolerates_the_rest() {
    let members = [
        member(&[(10, 50)], &[]),
        member(&[(30, 40)], &[]),
        member(&[(35, 60)], &[]),
    ];
    // At least 2 of 3: down only while two are down at once, 30 to 50.
    let measured = combine(day(), Rule::AtLeast(2), &members, &Spans::new());
    assert_eq!(measured.timeline.down, s(&[(30, 50)]));
    // At least 1: down only while all three are, 35 to 40.
    let one = combine(day(), Rule::AtLeast(1), &members, &Spans::new());
    assert_eq!(one.timeline.down, s(&[(35, 40)]));
    // More than there are members means all of them.
    let many = combine(day(), Rule::AtLeast(9), &members, &Spans::new());
    assert_eq!(many.timeline.down, s(&[(10, 60)]));
}

#[test]
fn members_under_maintenance_count_as_up() {
    // One of three web servers patched while another fails: still 2 up
    // by the rule, nothing lost.
    let members = [
        member(&[], &[(100, 160)]),
        member(&[(120, 130)], &[]),
        member(&[], &[]),
    ];
    let measured = combine(day(), Rule::AtLeast(2), &members, &Spans::new());
    assert_eq!(measured.down, 0.0);
    // During the failure only maintenance keeps it short of two: planned.
    assert_eq!(measured.timeline.maintenance, s(&[(120, 130)]));
    // With all of them needed, the window itself is the service's.
    let all = combine(day(), Rule::All, &members, &Spans::new());
    assert_eq!(all.timeline.maintenance, s(&[(100, 120), (130, 160)]));
    assert_eq!(all.timeline.down, s(&[(120, 130)]));
}

#[test]
fn an_empty_service_is_up() {
    let measured = combine(day(), Rule::All, &[], &Spans::new());
    assert_eq!(measured.availability(), Some(1.0));
}

#[test]
fn time_the_central_server_was_not_running_is_left_out() {
    // Critical from 100 on, with the central server stopped from 160 to
    // 400 and the incident resolved when it came back: only 60 minutes
    // were seen down, and the 240 minutes nobody watched are not counted.
    let sources = [source(&[(100, 400)], &[], "memory")];
    let gaps = s(&[(160, 400)]);
    let measured = measure(day(), &Spans::new(), &gaps, &sources);
    assert_eq!(measured.down, 60.0 * 60.0);
    assert_eq!(measured.unobserved, 240.0 * 60.0);
    assert_eq!(measured.counted, (1440.0 - 240.0) * 60.0);
    assert_eq!(measured.outages.len(), 1);
    assert_eq!(measured.outages[0].end, t(160));
    let expected = 1.0 - 60.0 / 1200.0;
    assert!((measured.availability().unwrap() - expected).abs() < 1e-12);
    // Planned maintenance over a gap stays maintenance, counted once.
    let planned = measure(day(), &s(&[(300, 500)]), &gaps, &sources);
    assert_eq!(planned.maintenance, 200.0 * 60.0);
    assert_eq!(planned.unobserved, 140.0 * 60.0);
    // A day with the central server down all along is not measured.
    let none = measure::<&str>(day(), &Spans::new(), &s(&[DAY]), &[]);
    assert_eq!(none.availability(), None);
}

#[test]
fn a_service_is_not_counted_while_the_central_server_was_not_running() {
    let members = [member(&[(10, 20)], &[]), member(&[], &[])];
    let measured = combine(day(), Rule::All, &members, &s(&[(15, 100)]));
    assert_eq!(measured.timeline.down, s(&[(10, 15)]));
    assert_eq!(measured.unobserved, 85.0 * 60.0);
}

#[test]
fn the_error_budget_of_a_target() {
    // 99.9 % of 30 days allows 43.2 minutes.
    let month = 30.0 * 86_400.0;
    assert!((error_budget(99.9, month, 0.0) - 2592.0).abs() < 1e-6);
    assert!((error_budget(99.9, month, 3000.0) + 408.0).abs() < 1e-6);
}
