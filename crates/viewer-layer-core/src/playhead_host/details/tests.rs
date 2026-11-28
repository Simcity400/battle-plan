use crate::{
    layers::View,
    playhead_host::{
        MapSide, open,
        views::tests::{hourly_steps, record},
        shared_maps_tests::Reads,
    },
};
use replay_data::{rows::TableId, scope::CaptureMode};
use std::sync::Arc;

#[test]
fn the_detail_report_reads_every_tables_status_and_counts_rows_even_when_the_map_does_not_need_them() {
    let entry = open(record("details", 8), hourly_steps()).unwrap();
    entry.set_capture(CaptureMode::Full);
    let (layer, _) = Reads::new(&[TableId::Countries]);
    let mut side = MapSide::new(Arc::clone(&entry), vec![layer], View::default()).unwrap();
    assert_eq!(super::of_map(&side), vec!["At the playback head: no hour reached yet".to_owned()]);
    let line = |lines: &[String], table: TableId| lines.iter().find(|line| line.starts_with(&format!("{table:?}: "))).cloned().unwrap();
    for (hour, countries) in [
        (1, "Countries: 3 rows".to_owned()),
        (4, "Countries: truncated, 4 missing".to_owned()),
        (5, "Countries: unavailable (the collector could not read it)".to_owned()),
        (6, format!("Countries: not recorded in a {} recording", CaptureMode::Full)),
    ] {
        side.anchor(hour, false, 0.0, true);
        side.catch_up(hour).unwrap();
        let lines = super::of_map(&side);
        assert_eq!(lines.len(), TableId::ALL.len() + 2, "every table, the head and the capture time");
        assert_eq!(lines[0], format!("At the playback head: hour {hour}"));
        assert_eq!(line(&lines, TableId::Countries), countries, "hour {hour}");
        assert_eq!(line(&lines, TableId::LandCombats), "LandCombats: 2 rows", "a table no layer reads still gives its row count");
        assert!(side.parts().unwrap().0.get(TableId::LandCombats).is_none(), "reporting its count does not keep a mirror");
        assert_eq!(lines.last().map(String::as_str), Some("Capture time: not recorded in the playhead's units"), "missing, never zero");
        for (line, table) in lines[1..lines.len() - 1].iter().zip(TableId::ALL) {
            assert!(line.starts_with(&format!("{table:?}: ")), "{line}");
            assert!(!line.contains(": 0 rows") || table == TableId::Countries, "a table not held is never zero rows: {line}");
        }
    }
}
