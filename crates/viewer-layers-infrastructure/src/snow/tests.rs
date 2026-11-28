
use super::{BAND, Changed, Made, RUN, Snow, Target};
use viewer_layer_core::layers::Layer;
use map_display_data::SnowMud;
use std::{io, ops::Range, sync::Arc, time::Duration};
use viewer_runtime::presentation::Continuity;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 200;

#[derive(Default)]
struct Counted {
    texels: Vec<u8>,
    held: Option<[u32; 2]>,
    wholes: usize,
    rects: Vec<(Range<u32>, Range<u32>)>,
    written: usize,
    begun: usize,
    finished: usize,
}

impl Target for Counted {
    fn takes_parts(&self, grid: &SnowMud, continuity: Continuity) -> bool {
        continuity == Continuity::Reset || (self.held == Some([grid.width, grid.height]) && grid.refresh.is_none())
    }

    fn whole(&mut self, grid: &Arc<SnowMud>, _continuity: Continuity) -> io::Result<()> {
        self.texels = grid.texels.to_vec();
        self.held = Some([grid.width, grid.height]);
        self.wholes += 1;
        self.written += self.texels.len();
        Ok(())
    }

    fn begin(&mut self, grid: &SnowMud, _continuity: Continuity) -> io::Result<()> {
        self.texels.resize(grid.texels.len(), 0);
        self.held = Some([grid.width, grid.height]);
        self.begun += 1;
        Ok(())
    }

    fn rect(&mut self, grid: &SnowMud, rows: Range<u32>, columns: Range<u32>) -> io::Result<()> {
        for row in rows.clone() {
            let at = (row * grid.width + columns.start) as usize * 4;
            let end = at + columns.len() * 4;
            for byte in at..end {
                self.texels[byte] = grid.texels[byte];
            }
            self.written += end - at;
        }
        self.rects.push((rows, columns));
        Ok(())
    }

    fn finish(&mut self, _grid: &Arc<SnowMud>, _continuity: Continuity) -> io::Result<()> {
        self.finished += 1;
        Ok(())
    }
}

fn grid(seed: u8) -> Arc<SnowMud> {
    let texels: Vec<u8> = (0..WIDTH * HEIGHT).flat_map(|cell| [seed, cell as u8, cell as u8, seed]).collect();
    Arc::new(SnowMud {
        width: WIDTH,
        height: HEIGHT,
        texels: texels.into(),
        unknown: vec![0; (WIDTH * HEIGHT) as usize].into(),
        province_snow: vec![].into(),
        post_factors: None,
        build: Duration::ZERO,
        summary: Default::default(),
        refresh: None,
    })
}

fn edited(grid: &SnowMud, cells: &[u32], value: u8) -> Arc<SnowMud> {
    let texels = grid.texels.edited(cells.iter().flat_map(|&cell| (0..4).map(move |channel| (cell as usize * 4 + channel, value))));
    Arc::new(SnowMud { texels, ..grid.clone() })
}

fn made(grid: &Arc<SnowMud>, cells: Option<&[u32]>) -> viewer_runtime::landed::Landed<Made> {
    made_at(None, grid, cells)
}

fn made_at(stamp: Option<i32>, grid: &Arc<SnowMud>, cells: Option<&[u32]>) -> viewer_runtime::landed::Landed<Made> {
    let (send, receive) = viewer_runtime::landed::channel();
    send.send(Made { epoch: 0, stamp, grid: Ok(Some(Arc::clone(grid))), changed: cells.map_or(Changed::Unknown, |cells| Changed::of_cells(cells, WIDTH)), refused: None, marked: [0; 3], held: 0, ns: 0 }).unwrap();
    receive.try_recv().unwrap()
}

fn layer() -> Snow {
    Snow::new(Err("no content: the upload alone is tested".into()))
}

fn sent(first: &Arc<SnowMud>, scene: &mut Counted) -> Snow {
    let mut layer = layer();
    layer.take(made(first, None));
    layer.push(scene).unwrap();
    assert!(layer.uploaded(), "an unlimited upload sends everything");
    assert_eq!(scene.texels, first.texels.to_vec(), "the scene holds the first grid");
    layer
}

#[test]
fn snow_upload_writes_only_the_band_a_changed_cell_lies_in() {
    let first = grid(1);
    let mut scene = Counted::default();
    let mut layer = sent(&first, &mut scene);
    assert_eq!((scene.wholes, scene.rects.len(), scene.written), (0, HEIGHT.div_ceil(BAND).div_ceil(RUN) as usize, first.texels.len()), "composed, every band is written once, a run of bands a piece");

    let cell = 13 * WIDTH + 10;
    let second = edited(&first, &[cell], 200);
    let (rects, written) = (scene.rects.len(), scene.written);
    layer.take(made(&second, Some(&[cell])));
    assert!(!layer.uploaded(), "the changed band is owed to the scene");
    layer.push(&mut scene).unwrap();
    assert_eq!(&scene.rects[rects..], [(8..16, 10..11)], "one band's one column, of {} bands", HEIGHT.div_ceil(BAND));
    assert_eq!(scene.written - written, BAND as usize * 4, "{} bytes, not the grid's {}", BAND * 4, first.texels.len());
    assert_eq!((scene.wholes, scene.finished), (0, 2));
    assert_eq!(scene.texels, second.texels.to_vec(), "the scene holds the new grid");
    assert!(layer.uploaded());
}

#[test]
fn snow_a_whole_change_is_written_in_runs_of_bands_and_ends_equal_to_whole() {
    let first = grid(1);
    let mut scene = Counted::default();
    let mut layer = sent(&first, &mut scene);
    let every: Vec<u32> = (0..WIDTH * HEIGHT).collect();
    let second = edited(&first, &every, 9);
    layer.take(made(&second, Some(&every)));
    let pieces = HEIGHT.div_ceil(BAND).div_ceil(RUN) as usize;
    let (rects, finished) = (scene.rects.len(), scene.finished);
    layer.push(&mut scene).unwrap();
    assert_eq!(scene.rects.len() - rects, pieces, "every band owed, a run of bands a piece");
    assert_eq!((scene.finished - finished, scene.wholes), (1, 0), "finished once, never one indivisible write");
    assert!(layer.uploaded(), "one upload owes nothing after it");
    assert_eq!(scene.texels, second.texels.to_vec(), "the pieces leave the grid, as a whole upload does");

    let cell = 2 * WIDTH + 3;
    let third = edited(&second, &[cell], 44);
    layer.take(made(&third, Some(&[cell])));
    let rects = scene.rects.len();
    layer.push(&mut scene).unwrap();
    assert_eq!(&scene.rects[rects..], [(0..8, 3..4)], "the grid after it writes its own band");
    assert_eq!(scene.texels, third.texels.to_vec(), "the scene holds the newest grid");
    assert_eq!(scene.begun, 3, "a run for the composed grid, one for the change, one for the grid after it");
}

#[test]
fn snow_two_grids_taken_in_one_frame_upload_the_union_of_their_bands() {
    let first = grid(1);
    let mut scene = Counted::default();
    let mut layer = sent(&first, &mut scene);
    let (early, late) = (3 * WIDTH + 4, 30 * WIDTH + 50);
    let second = edited(&first, &[early], 70);
    let third = edited(&second, &[late], 90);
    let fourth = edited(&third, &[early], 110);
    layer.fed.receive(Some(4), made_at(Some(4), &second, Some(&[early])));
    layer.fed.receive(Some(8), made_at(Some(8), &third, Some(&[late])));
    layer.fed.receive(Some(12), made_at(Some(12), &fourth, Some(&[early])));
    layer.fed.applied_through(8);
    layer.exchange(false).unwrap();
    let rects = scene.rects.len();
    layer.push(&mut scene).unwrap();
    let mut written = scene.rects[rects..].to_vec();
    written.sort_by_key(|(rows, _)| rows.start);
    assert_eq!(written, [(0..8, 4..5), (24..32, 50..51)], "the first grid's band and the second's, not only the second's");
    assert_eq!(scene.wholes, 0);
    assert_eq!(scene.texels, third.texels.to_vec(), "the scene equals a whole write of the second grid, not the one made ahead of it");
    assert!(layer.uploaded());
}

#[test]
fn snow_changed_sets_join_and_are_written_in_runs_of_neighbouring_bands() {
    let cells = [WIDTH * 9 + 5, WIDTH * 15 + 30, 3, WIDTH * 39 + 63];
    let mut changed = Changed::of_cells(&cells, WIDTH);
    assert_eq!(changed, Changed::Bands(vec![3..4, 5..31, 0..0, 0..0, 63..64]));
    changed.join(Changed::of_cells(&[WIDTH * 17 + 40], WIDTH));
    assert_eq!(changed, Changed::Bands(vec![3..4, 5..31, 40..41, 0..0, 63..64]), "a third band, joined");
    let size = [WIDTH, 40];
    assert_eq!(changed.run_in(size, None), Some((0..24, 3..41)), "bands 0 to 2 together, their columns' bound");
    changed.written(0..24, size);
    assert_eq!(changed.run_in(size, None), Some((32..40, 63..64)));
    changed.written(32..40, size);
    assert_eq!((changed.run_in(size, None), changed.bands(40)), (None, 0));
    changed.join(Changed::Unknown);
    assert_eq!(changed, Changed::Unknown, "not known absorbs");
    changed.join(Changed::of_cells(&[1], WIDTH));
    assert_eq!((changed.run_in(size, None), changed.bands(40)), (Some((0..40, 0..WIDTH)), 5), "not known is written whole");
}

#[test]
fn snow_writes_the_bands_the_camera_shows_and_owes_the_rest() {
    let shown = super::Shown { rows: 80..100, columns: [20..30, 0..0] };
    let mut changed = Changed::of_cells(&[90 * WIDTH + 25], WIDTH);
    changed.join(Changed::of_cells(&[8 * WIDTH + 25], WIDTH));
    let size = [WIDTH, HEIGHT];
    assert_eq!(
        changed.run_in(size, Some(&shown)),
        Some((88..96, 25..26)),
        "the band of the row in view, and nothing of the band at row 8",
    );
    changed.written(88..96, size);
    assert_eq!(changed.run_in(size, Some(&shown)), None, "what is left is outside the view");
    assert_eq!(changed.run_in(size, None), Some((8..16, 25..26)), "and is written when nothing limits it");

    let seam = super::Shown { rows: 0..HEIGHT, columns: [60..WIDTH, 0..4] };
    let changed = Changed::of_cells(&[WIDTH + 2, WIDTH + 40, WIDTH + 62], WIDTH);
    assert_eq!(
        changed.run_in(size, Some(&seam)),
        Some((0..8, 2..63)),
        "a view across the seam shows the columns at both ends",
    );
    assert!(!seam.shows(0, &(40..41)), "and not the columns between them");
}

#[test]
fn snow_only_the_frames_upload_writes_what_the_worker_folded() {
    let first = grid(1);
    let mut scene = Counted::default();
    let mut layer = sent(&first, &mut scene);
    let cell = 101 * WIDTH + 17;
    let second = edited(&first, &[cell], 88);
    let (wholes, rects, written, finished) = (scene.wholes, scene.rects.len(), scene.written, scene.finished);

    layer.fed.receive(Some(4), made_at(Some(4), &second, Some(&[cell])));
    layer.fed.applied_through(4);
    layer.exchange(false).unwrap();
    assert_eq!(
        (scene.wholes, scene.rects.len(), scene.written, scene.finished),
        (wholes, rects, written, finished),
        "taking the worker's grid writes no texel: only the upload does",
    );

    layer.push(&mut scene).unwrap();
    assert_eq!(&scene.rects[rects..], [(96..104, 17..18)], "the upload writes the band the fold changed");
    assert_eq!(scene.texels, second.texels.to_vec(), "the scene holds the grid the worker made");
    assert!(layer.uploaded());
}
