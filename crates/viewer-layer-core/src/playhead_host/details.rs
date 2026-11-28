#[cfg(test)]
mod tests;

use super::{Entry, MapSide, mirror::Mirrors};
mod reasons;
#[cfg(any(test, feature = "test-support"))]
pub use reasons::BEING_READ;
use replay_columnar::{
    manifest::Status,
    unit::{BLOCK, reader},
};
use replay_data::{Hour, rows::TableId};
use std::{io, sync::Arc, time::Duration};
use viewer_foundation::tables::Seen;

pub const REPORT_WAIT: Duration = Duration::from_secs(60);

pub fn of_map(side: &MapSide<impl crate::playhead_host::PlaybackProducts>) -> Vec<String> {
    lines(side.entry(), side.applied(), side.parts().map(|(mirrors, _)| mirrors))
}

pub fn lines(entry: &Entry, hour: Option<Hour>, mirrors: Option<&Mirrors>) -> Vec<String> {
    let Some(hour) = hour else {
        return vec!["At the playback head: no hour reached yet".to_owned()];
    };
    let mut lines = Vec::with_capacity(TableId::ALL.len() + 2);
    lines.push(format!("At the playback head: hour {hour}"));
    let written = entry.pages().written_tables(&super::timeline_runs(entry), TableId::ALL.len());
    for table in TableId::ALL {
        if let playhead::pages::TableWritten::Withheld(reason) = &written[table.index()] {
            lines.push(format!("{table:?}: unavailable ({reason})"));
            continue;
        }
        let held = mirrors.and_then(|mirrors| mirrors.get(table)).filter(|mirror| mirror.head() == Some(hour));
        lines.push(match status(entry, table, hour) {
            Err(error) => format!("{table:?}: its status cannot be read at the head ({error})"),
            Ok(None) => format!("{table:?}: not recorded here: a gap in the recording"),
            Ok(Some((Status::Complete, (run, _)))) => match held.map_or_else(|| row_count(entry, table, run, hour), |mirror| Ok(mirror.rows().count())) {
                Ok(rows) => format!("{table:?}: {rows} rows"),
                Err(error) => format!("{table:?}: recorded; its rows cannot be counted at the head ({error})"),
            },
            Ok(Some((Status::Truncated { missing }, _))) => format!("{table:?}: truncated, {missing} missing"),
            Ok(Some((Status::Unavailable { reason }, place))) => format!("{table:?}: unavailable ({})", reasons::words(entry, table, reason, place, REPORT_WAIT)),
            Ok(Some((Status::NotRecorded, _))) => match entry.capture.get() {
                Some(mode) => format!("{table:?}: not recorded in a {mode} recording"),
                None => format!("{table:?}: not recorded at this hour"),
            },
        });
    }
    lines.push("Capture time: not recorded in the playhead's units".to_owned());
    lines
}

fn row_count(entry: &Entry, table: TableId, run: usize, hour: Hour) -> io::Result<usize> {
    let mut lane = playhead::lane::Lane::new(Arc::clone(entry.pages()), run, table.index(), &[], false)?;
    lane.seek(hour)?;
    Ok(match lane.log() {
        Some(log) => log.live().ones().count(),
        None => lane.cells().rows().count(),
    })
}

pub fn differ(side: &MapSide<impl crate::playhead_host::PlaybackProducts>, today: &replay_frame::frame::DecodedFrame) -> (u64, u64, Option<String>, Vec<TableId>) {
    use replay_frame::frame::TableView;
    let Some(hour) = side.applied() else { return (1, 1, Some("the head has reached no hour".into()), Vec::new()) };
    let (entry, mirrors) = (side.entry(), side.parts().map(|(mirrors, _)| mirrors));
    let (mut entries, mut differing, mut first, mut absent) = (0, 0, None, Vec::new());
    let written = entry.pages().written_tables(&super::timeline_runs(entry), TableId::ALL.len());
    for table in TableId::ALL {
        let held = mirrors.and_then(|mirrors| mirrors.get(table));
        let view = &*today.tables[table.index()];
        if let playhead::pages::TableWritten::Withheld(reason) = &written[table.index()] {
            entries += 1;
            let same = matches!(view, TableView::Unavailable { reason: today } if {
                today == reason
                    || today
                        .strip_suffix(reason.as_str())
                        .is_some_and(|prefix| prefix == format!("{table:?} of the recording: "))
            });
            if !same {
                differing += 1;
                first.get_or_insert_with(|| format!("{table:?} at {hour}: withheld ({reason})"));
            }
            continue;
        }
        let ours = status(entry, table, hour).ok().flatten().map(|(status, _)| status);
        let agree = match (ours, view) {
            (Some(Status::Complete), TableView::Complete(rows)) => held.is_none_or(|mirror| mirror.rows().count() == rows.len()),
            (Some(Status::Truncated { missing }), TableView::Truncated { missing: today, .. }) => missing == *today,
            (Some(Status::Unavailable { .. }), TableView::Unavailable { reason }) => held.and_then(|mirror| mirror.reason()).is_none_or(|text| text == reason),
            (Some(Status::NotRecorded) | None, TableView::NotRecorded { .. }) => true,
            _ => false,
        };
        if !agree && ours == Some(Status::NotRecorded) && !held_at_all(entry, table) {
            absent.push(table);
            continue;
        }
        entries += 1;
        if !agree {
            differing += 1;
            first.get_or_insert_with(|| format!("{table:?} at {hour}: at the head {ours:?} ({:?} rows held), today {}", held.map(|mirror| mirror.rows().count()), match view {
                TableView::Complete(rows) => format!("{} rows", rows.len()),
                TableView::Truncated { missing, .. } => format!("truncated, {missing} missing"),
                TableView::Unavailable { reason } => format!("unavailable ({reason})"),
                TableView::NotRecorded { mode } => format!("not recorded in a {mode} recording"),
            }));
        }
    }
    (entries, differing, first, absent)
}

pub fn held_at_all(entry: &Entry, table: TableId) -> bool {
    entry.pages().table_has_activity(table.index())
}

pub fn seen(entry: &Entry, table: TableId, hour: Hour, wait: Duration) -> Seen {
    if entry.pages().kinds(table.index()).is_none()
        && entry.pages().held(table.index()) > 0
        && let Some(piece) = entry.playhead().timeline().pieces().iter().find(|piece| piece.first <= hour && hour <= piece.last)
        && let Some(reason) = entry.pages().withheld_at(piece.run, table.index(), hour)
    {
        return Seen::Unavailable(reason.to_owned());
    }
    match status(entry, table, hour) {
        Ok(Some((Status::Complete, _))) => Seen::Complete,
        Ok(Some((Status::Truncated { missing }, _))) => Seen::Truncated(missing),
        Ok(Some((Status::Unavailable { reason: id }, place))) => Seen::Unavailable(reasons::words(entry, table, id, place, wait)),
        Ok(Some((Status::NotRecorded, _))) | Ok(None) => Seen::NotRecorded(entry.capture.get().copied()),
        Err(error) => Seen::Unavailable(format!("its status cannot be read at the head ({error})")),
    }
}

fn status(entry: &Entry, table: TableId, hour: Hour) -> io::Result<Option<(Status, (usize, usize))>> {
    let Some(piece) = entry.playhead().timeline().pieces().iter().find(|piece| piece.first <= hour && hour <= piece.last) else {
        return Ok(None);
    };
    if entry.pages().kinds(table.index()).is_none() {
        return Ok(Some((Status::NotRecorded, (piece.run, 0))));
    }
    let run = entry.pages().runs().get(piece.run).ok_or_else(|| io::Error::other(format!("no run {}", piece.run)))?;
    let at = hour.checked_sub(run.first_hour).and_then(|at| u32::try_from(at).ok()).ok_or_else(|| io::Error::other(format!("hour {hour} is outside run {}", piece.run)))?;
    let block = at / BLOCK;
    let held = entry.pages().table_entry(piece.run, block as usize, table.index())?;
    let start = block * BLOCK;
    let (status, _, _, _) = reader::status(&held, (at - start) as u16, run.first_hour + start as Hour)?;
    Ok(Some((status, (piece.run, block as usize))))
}
