use crate::layers::{BOUND, Published, Settling};
use replay_data::Hour;
use std::{collections::VecDeque, io, time::Instant};

const KEPT: usize = 64;

const STOPPED: u32 = 4;

#[derive(Default)]
struct Standing {
    at: Option<Hour>,
    last: Option<Hour>,
    fell: u64,
    behind: u32,
    publications: u64,
    stood: u64,
    held: u64,
    worst: u32,
    live_settles: u64,
    live_yields: u64,
    behind_hours: Vec<f64>,
    behind_publications: Vec<u32>,
    behind_ms: Vec<f64>,
}

pub struct Standings {
    layers: Vec<Standing>,
    publications: VecDeque<(Hour, Instant)>,
    hour: Option<Hour>,
    stopped: u32,
    check: bool,
    check_rates: bool,
    judged: bool,
    judges: u64,
}

fn checking() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BATTLEPLAN_VIEWER_CHECK_STEP").or_else(|| std::env::var_os("HOI4_VIEWER_CHECK_STEP")).is_some())
}

fn checking_rates() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BATTLEPLAN_VIEWER_CHECK_RATES").or_else(|| std::env::var_os("HOI4_VIEWER_CHECK_RATES")).is_some())
}

impl Standings {
    pub fn new(layers: usize) -> Self {
        Self { layers: (0..layers).map(|_| Standing::default()).collect(), publications: VecDeque::new(), hour: None, stopped: 0, check: checking(), check_rates: checking_rates(), judged: false, judges: 0 }
    }

    pub fn reset(&mut self) {
        for layer in &mut self.layers {
            (layer.at, layer.last, layer.behind) = (None, None, 0);
        }
        self.publications.clear();
        (self.hour, self.stopped, self.judged) = (None, 0, false);
    }

    pub fn step(&mut self, hour: Option<Hour>, now: Instant) {
        let moved = hour.is_some() && hour != self.hour;
        if let Some(hour) = hour.filter(|_| moved) {
            if self.publications.len() == KEPT {
                self.publications.pop_front();
            }
            self.publications.push_back((hour, now));
            self.judged = false;
        }
        (self.hour, self.stopped) = (hour, if moved { 0 } else { self.stopped.saturating_add(1) });
    }

    #[cfg(test)]
    fn behind(&self, at: usize) -> u32 {
        self.layers.get(at).map_or(0, |layer| layer.behind)
    }

    pub fn hours_stopped(&self) -> bool {
        self.stopped > STOPPED
    }

    pub fn settled(&mut self, at: usize, name: &'static str, published: Published, now: Instant, bound: u32, settling: Settling) -> io::Result<()> {
        if at >= self.layers.len() {
            self.layers.resize_with(at + 1, Standing::default);
        }
        let live = !settling.waits() && self.hour.is_some();
        self.layers[at].live_settles += u64::from(live);
        let (stands, product) = match published {
            Published::Current => (self.hour, false),
            Published::At(hour) | Published::Cadence(hour) => (Some(hour), true),
            Published::Nothing => {
                let layer = &mut self.layers[at];
                layer.behind = 0;
                if live {
                    layer.live_yields += 1;
                    viewer_runtime::view_record::add("live layer settlements behind", name, 1);
                    viewer_runtime::view_record::add("live layers without a product", name, 1);
                }
                return Ok(());
            }
        };
        let (behind, since) = self.standing(stands);
        let record = viewer_runtime::view_record::on();
        let (check, hour) = (self.check || (bound != BOUND && self.check_rates), self.hour);
        let layer = &mut self.layers[at];
        if live && let Some((head, stands)) = hour.zip(stands).filter(|(head, stands)| head > stands) {
            layer.live_yields += 1;
            if record {
                let hours = head.abs_diff(stands);
                layer.behind_hours.push(f64::from(hours));
                viewer_runtime::view_record::add("live layer settlements behind", name, 1);
                viewer_runtime::view_record::set("live layer hours behind", name, u64::from(hours));
            }
        }
        if let (Some(product_at), Some(stands)) = (layer.at, stands)
            && stands < product_at
        {
            if product && check {
                return Err(io::Error::other(format!("The {name} layer's product went backwards: it stood at {product_at} and now stands at {stands}")));
            }
            layer.fell += u64::from(!product);
        } else if let (Some(last), Some(stands)) = (layer.last, stands)
            && stands < last
        {
            layer.fell += 1;
        }
        layer.last = stands.or(layer.last);
        if product {
            layer.at = stands.max(layer.at);
        }
        layer.behind = behind;
        layer.publications += 1;
        layer.worst = layer.worst.max(behind);
        if behind > 0 {
            layer.stood += 1;
            if record {
                layer.behind_publications.push(behind);
                layer.behind_ms.push(since.map_or(0.0, |since| now.saturating_duration_since(since).as_secs_f64() * 1e3));
            }
        }
        if behind >= bound {
            layer.held += 1;
            if check && behind > bound {
                return Err(io::Error::other(format!("The {name} layer's product stood {behind} publications behind the publication's hour {hour:?}, past the bound of {bound}: it stands at {stands:?}")));
            }
        }
        Ok(())
    }

    pub fn settled_whole(&mut self, at: usize, published: Published) {
        if at >= self.layers.len() {
            self.layers.resize_with(at + 1, Standing::default);
        }
        let stands = match published {
            Published::Current => self.hour,
            Published::At(hour) | Published::Cadence(hour) => Some(hour),
            Published::Nothing => None,
        };
        self.layers[at].behind = self.standing(stands).0;
    }

    fn standing(&self, stands: Option<Hour>) -> (u32, Option<Instant>) {
        let Some(stands) = stands else { return (0, None) };
        let mut count = 0_u32;
        let mut since: Option<Instant> = None;
        for &(hour, at) in &self.publications {
            if hour > stands {
                count += 1;
                since = Some(since.map_or(at, |held| held.min(at)));
            }
        }
        (count, since)
    }

    pub fn stopped_whole(&mut self, drawn: &[(usize, &'static str)]) -> io::Result<()> {
        if !(self.check || self.check_rates) || self.judged || !self.hours_stopped() {
            return Ok(());
        }
        (self.judged, self.judges) = (true, self.judges + 1);
        for &(at, name) in drawn {
            let Some(layer) = self.layers.get(at) else { continue };
            if layer.behind > 0 {
                return Err(io::Error::other(format!(
                    "The hours have stopped for {} publications and the {name} layer's product still stands {} publications behind {:?}, at {:?}",
                    self.stopped, layer.behind, self.hour, layer.at
                )));
            }
        }
        Ok(())
    }

    pub fn report(&self, names: &[&'static str]) -> serde_json::Value {
        use serde_json::json;
        let each: Vec<serde_json::Value> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.publications > 0 || layer.live_yields > 0)
            .map(|(at, layer)| {
                let mut behind: Vec<f64> = layer.behind_publications.iter().map(|&behind| f64::from(behind)).collect();
                let mut ms = layer.behind_ms.clone();
                let mut hours = layer.behind_hours.clone();
                json!({
                    "name": names.get(at).copied().unwrap_or("?"),
                    "publications": layer.publications,
                    "publications_behind": layer.stood,
                    "share_behind": layer.stood as f64 / layer.publications.max(1) as f64,
                    "live_settles": layer.live_settles,
                    "live_settles_behind": layer.live_yields,
                    "live_hours_behind": distribution(&mut hours),
                    "held": layer.held,
                    "worst_publications_behind": layer.worst,
                    "fell_without_a_product_going_back": layer.fell,
                    "behind_publications": distribution(&mut behind),
                    "behind_ms": distribution(&mut ms),
                })
            })
            .collect();
        json!({
            "bound": BOUND,
            "stopped_pictures_judged": self.judges,
            "layers": each,
            "scope": "Per layer, over the publications it settled in (one publication is the map side's step, which applies one of the playhead's with that step's own hourly change sets): publications_behind, the publications at which its product stood behind the publication's hour, with how far behind it stood at those in publications and in milliseconds; held, the publications at which it reached the bound and the publication waited for it by name; fell_without_a_product_going_back, the publications at which the hour it stood at fell because it had owed nothing the publication before and then drew something of an older hour. A layer with no worker of its own is current at every publication and shows zeros. stopped_pictures_judged counts the times the judge (BATTLEPLAN_VIEWER_CHECK_STEP=1) reached a picture whose hours had stopped and found every drawn layer standing at it.",
        })
    }
}

fn distribution(values: &mut [f64]) -> serde_json::Value {
    use serde_json::json;
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |share: f64| values.get(((values.len() as f64 * share) as usize).min(values.len().saturating_sub(1))).copied();
    json!({"p50": at(0.5), "p90": at(0.9), "p99": at(0.99), "max": values.last().copied(), "count": values.len()})
}

#[cfg(test)]
mod tests {
    use super::{KEPT, Standings};
    use crate::layers::{BOUND, Published, Rate, Settling};
    use std::time::{Duration, Instant};

    #[test]
    fn only_paused_and_whole_settles_wait() {
        assert!(Settling::step().waiting(Duration::from_secs(5)).is_zero());
        assert!(Settling::still().waits() && !Settling::still().strict());
        assert!(Settling::Whole.waits() && Settling::Whole.strict());
    }

    #[test]
    fn a_held_product_stands_behind_until_it_catches_up() {
        let mut standings = Standings::new(1);
        let now = Instant::now();
        standings.step(Some(10), now);
        standings.settled(0, "counters", Published::Current, now, BOUND, Settling::step()).unwrap();
        assert_eq!(standings.behind(0), 0);
        for step in 1..=BOUND {
            standings.step(Some(10 + step as i32), now);
            standings.settled(0, "counters", Published::At(10), now, BOUND, Settling::step()).unwrap();
            assert_eq!(standings.behind(0), step, "its product has stood at hour 10 for {step} publications");
        }
        assert_eq!(standings.layers[0].live_yields, u64::from(BOUND));
        standings.step(Some(11 + BOUND as i32), now);
        standings.settled(0, "counters", Published::Current, now, BOUND, Settling::step()).unwrap();
        assert_eq!(standings.behind(0), 0, "it is current again");
    }

    #[test]
    fn a_rated_product_uses_its_own_bound() {
        let rate = Rate::a_second(4);
        let bound = rate.behind();
        assert!(bound > BOUND, "a rate of 4 a second covers more than one publication: {bound}");
        let mut standings = Standings::new(1);
        standings.check_rates = true;
        let now = std::time::Instant::now();
        for at in 0..=bound as i32 + 1 {
            standings.step(Some(at), now);
            let settled = standings.settled(0, "scripts at the head", Published::Cadence(0), now, bound, Settling::step());
            if at <= bound as i32 {
                settled.expect("inside its rate");
            } else {
                let why = settled.expect_err("past its rate").to_string();
                assert!(why.contains("scripts at the head") && why.contains(&format!("past the bound of {bound}")), "{why}");
            }
        }
    }

    #[test]
    fn a_product_that_has_made_nothing_yet_is_behind_nothing() {
        let mut standings = Standings::new(1);
        let now = Instant::now();
        for at in 0..10 {
            standings.step(Some(at), now);
            standings.settled(0, "units", Published::Nothing, now, BOUND, Settling::step()).unwrap();
            assert_eq!(standings.behind(0), 0);
        }
    }

    #[test]
    fn the_judge_refuses_a_product_past_the_bound_and_one_that_went_backwards() {
        let mut standings = Standings::new(1);
        standings.check = true;
        let now = Instant::now();
        for at in 0..=BOUND as i32 + 1 {
            standings.step(Some(at), now);
            let settled = standings.settled(0, "sounds", Published::At(0), now, BOUND, Settling::step());
            if at <= BOUND as i32 {
                settled.expect("inside the bound");
            } else {
                let why = settled.expect_err("past the bound").to_string();
                assert!(why.contains("sounds") && why.contains("past the bound"), "{why}");
            }
        }
        let mut standings = Standings::new(1);
        standings.check = true;
        standings.step(Some(5), now);
        standings.settled(0, "ships", Published::At(5), now, BOUND, Settling::step()).unwrap();
        standings.step(Some(6), now);
        let why = standings.settled(0, "ships", Published::At(4), now, BOUND, Settling::step()).expect_err("backwards").to_string();
        assert!(why.contains("ships") && why.contains("went backwards"), "{why}");
    }

    #[test]
    fn a_layer_that_owed_nothing_and_then_draws_an_older_hour_is_counted_not_refused() {
        let mut standings = Standings::new(1);
        standings.check = true;
        let now = Instant::now();
        standings.step(Some(20), now);
        standings.settled(0, "ships", Published::Current, now, BOUND, Settling::step()).expect("it draws nothing and owes nothing");
        standings.step(Some(21), now);
        standings.settled(0, "ships", Published::Cadence(18), now, BOUND, Settling::step()).expect("it now draws something of an older hour");
        assert_eq!(standings.layers[0].fell, 1, "the fall is counted");
        let why = standings.settled(0, "ships", Published::Cadence(17), now, BOUND, Settling::step()).expect_err("a product going back").to_string();
        assert!(why.contains("went backwards"), "{why}");
    }

    #[test]
    fn the_judge_refuses_a_layer_that_has_not_caught_up_once_the_hours_have_stopped() {
        let mut standings = Standings::new(1);
        standings.check = true;
        let now = Instant::now();
        standings.step(Some(10), now);
        standings.settled(0, "buildings", Published::At(8), now, BOUND, Settling::step()).unwrap();
        for _ in 0..=super::STOPPED {
            standings.step(Some(10), now);
            standings.settled(0, "buildings", Published::At(8), now, BOUND, Settling::step()).unwrap();
        }
        assert!(standings.hours_stopped(), "the hours have not moved for more than the bound");
        let why = standings.stopped_whole(&[(0, "buildings")]).expect_err("the hours stopped and it is behind").to_string();
        assert!(why.contains("buildings") && why.contains("stopped"), "{why}");
        standings.judged = false;
        standings.settled(0, "buildings", Published::At(10), now, BOUND, Settling::step()).unwrap();
        standings.stopped_whole(&[(0, "buildings")]).expect("caught up, the stopped picture is whole");
    }

    #[test]
    fn a_seek_starts_the_standing_over_so_the_picture_before_is_never_read_as_going_backwards() {
        let mut standings = Standings::new(1);
        standings.check = true;
        let now = Instant::now();
        for at in 100..100 + BOUND as i32 {
            standings.step(Some(at), now);
            standings.settled(0, "counters", Published::At(at - 1), now, BOUND, Settling::step()).unwrap();
        }
        standings.reset();
        assert_eq!(standings.behind(0), 0, "nothing stands behind the picture a seek has not made yet");
        for at in 20..20 + BOUND as i32 {
            standings.step(Some(at), now);
            standings.settled(0, "counters", Published::At(at - 1), now, BOUND, Settling::step()).expect("the sought picture is its own");
            assert_eq!(standings.behind(0), 1);
        }
    }

    #[test]
    fn paused_and_resumed_every_layer_reaches_the_hour_shown_and_falls_behind_again_when_the_hours_run() {
        let mut standings = Standings::new(1);
        standings.check = true;
        let now = Instant::now();
        for at in 1..=10 {
            standings.step(Some(at), now);
            standings.settled(0, "sounds", Published::At(at - 1), now, BOUND, Settling::step()).unwrap();
        }
        standings.step(Some(10), now);
        standings.settled(0, "sounds", Published::At(10), now, BOUND, Settling::step()).unwrap();
        for _ in 0..=super::STOPPED {
            standings.step(Some(10), now);
            standings.settled(0, "sounds", Published::At(10), now, BOUND, Settling::step()).unwrap();
        }
        assert_eq!(standings.behind(0), 0, "stopped, it stands at the hour shown");
        standings.stopped_whole(&[(0, "sounds")]).expect("the stopped picture is whole");
        for at in 11..=20 {
            standings.step(Some(at), now);
            standings.settled(0, "sounds", Published::At(at - 1), now, BOUND, Settling::step()).unwrap();
            assert_eq!(standings.behind(0), 1);
        }
    }

    #[test]
    fn at_the_end_of_the_recording_the_hours_stop_and_every_layer_reaches_the_hour_shown() {
        let mut standings = Standings::new(2);
        standings.check = true;
        let now = Instant::now();
        standings.step(Some(60), now);
        standings.settled(0, "buildings", Published::At(59), now, BOUND, Settling::step()).unwrap();
        standings.settled(1, "political", Published::Current, now, BOUND, Settling::step()).unwrap();
        for step in 0..=super::STOPPED {
            standings.step(Some(60), now);
            let stands = if step < BOUND { Published::At(59) } else { Published::At(60) };
            standings.settled(0, "buildings", stands, now, BOUND, Settling::step()).unwrap();
            standings.settled(1, "political", Published::Current, now, BOUND, Settling::step()).unwrap();
        }
        standings.stopped_whole(&[(0, "buildings"), (1, "political")]).expect("the last hour is whole on every layer");
    }

    #[test]
    fn a_product_older_than_every_publication_kept_counts_as_the_publications_kept() {
        let mut standings = Standings::new(1);
        let now = Instant::now();
        for at in 0..KEPT as i32 * 2 {
            standings.step(Some(at), now);
        }
        standings.settled(0, "labels", Published::At(0), now, BOUND, Settling::step()).unwrap();
        assert_eq!(standings.behind(0), u32::try_from(KEPT).expect("the publications kept"), "only the publications kept are counted");
    }
}
