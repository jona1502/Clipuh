use clipper_editor::timeline::wheel_scroll_offset;

#[test]
fn wheel_pans_time_and_stops_at_both_edges() {
    assert_eq!(wheel_scroll_offset(100., 0., -48., 500.), 148.);
    assert_eq!(wheel_scroll_offset(100., 0., 48., 500.), 52.);
    assert_eq!(wheel_scroll_offset(100., -24., 0., 500.), 124.);
    assert_eq!(wheel_scroll_offset(100., -24., -48., 500.), 124.);
    assert_eq!(wheel_scroll_offset(10., 0., 48., 500.), 0.);
    assert_eq!(wheel_scroll_offset(490., 0., -48., 500.), 500.);
    assert_eq!(wheel_scroll_offset(0., 0., -48., 0.), 0.);
}
