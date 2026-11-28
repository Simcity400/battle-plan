
use crate::{changes::Changes, playhead::Reader, step::Step};
use replay_data::Hour;
use std::{collections::VecDeque, sync::Arc};

struct Seat {
    reader: Reader,
    next: u64,
    own: VecDeque<(u64, Arc<Step>)>,
    parked: Option<Hour>,
    whole: bool,
    asks: bool,
    lands_itself: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub(crate) seq: u64,
    pub(crate) first: Hour,
    pub(crate) last: Hour,
    pub(crate) gap: bool,
}

pub(crate) struct Ring {
    pub(crate) epoch: u64,
    size: usize,
    ahead: usize,
    budget: Option<Arc<crate::ByteBudget>>,
    next_seq: u64,
    built: VecDeque<Arc<Step>>,
    payload_offsets: VecDeque<usize>,
    lent: Vec<Arc<Step>>,
    consumers: Vec<Option<Seat>>,
    orphans: Vec<(Reader, u64, Arc<Step>)>,
    taken: Option<Hour>,
    reset: Option<(u64, Hour)>,
    published: Option<Hour>,
    published_in: u64,
    bound: usize,
}

fn whole(step: &Step) -> bool {
    step.tables.iter().all(|changes| changes.short() == 0 && changes.head().is_none_or(|head| head >= step.last))
}

fn next_of(built: &VecDeque<Arc<Step>>, next: u64) -> usize {
    built.partition_point(|step| step.seq < next)
}

fn place(own: &VecDeque<(u64, Arc<Step>)>, before: u64) -> usize {
    own.partition_point(|(held, _)| *held <= before)
}

fn retire(pool: &mut Vec<Arc<Step>>, reuse: bool, steps: impl IntoIterator<Item = Arc<Step>>) {
    if reuse { pool.extend(steps); } else { steps.into_iter().for_each(drop); }
}

impl Ring {
    pub(crate) fn new(size: usize) -> Self {
        Self { epoch: 0, size, ahead: usize::MAX, budget: None, next_seq: 0, built: VecDeque::new(), payload_offsets: VecDeque::from([0]), lent: Vec::new(), consumers: Vec::new(), orphans: Vec::new(), taken: None, reset: None, published: None, published_in: 0, bound: usize::MAX }
    }

    pub(crate) fn share_budget(&mut self, budget: Arc<crate::ByteBudget>) -> std::io::Result<()> {
        if self.budget.as_ref().is_some_and(|held| Arc::ptr_eq(held, &budget)) { return Ok(()); }
        if self.budget.is_some() { return Err(std::io::Error::other("The playhead already has a different payload budget")); }
        let mut steps = self.steps();
        steps.sort_unstable_by_key(Arc::as_ptr);
        steps.dedup_by(|a, b| Arc::ptr_eq(a, b));
        for step in steps {
            let mut charge = budget.charge();
            charge.resize(step.payload());
            *step.charge.lock().unwrap() = Some(charge);
        }
        self.budget = Some(budget);
        self.lent.clear();
        Ok(())
    }

    fn admit(&self, step: &Step) {
        if let Some(budget) = &self.budget {
            let mut charge = step.charge.lock().unwrap();
            if charge.is_none() {
                let mut new = budget.charge();
                new.resize(step.payload());
                *charge = Some(new);
            }
        }
    }

    pub(crate) fn room(&self, head: Hour) -> bool {
        let past = self.past(head);
        let untaken = next_of(&self.built, self.furthest().unwrap_or(0));
        past < self.size && self.built.len() - untaken < self.size && (untaken == self.built.len() || self.payload_between(untaken, self.built.len()) < self.ahead) && (past == 0 || (self.past_bytes(head) < self.ahead && self.budget.as_ref().is_none_or(|b| !b.full())))
    }

    pub(crate) fn resize(&mut self, size: usize, ahead: usize) {
        (self.size, self.ahead) = (size, ahead);
    }

    pub(crate) fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn lead(&self, head: Hour) -> (usize, usize, bool) {
        let (past, bytes) = (self.past(head), self.past_bytes(head));
        (past, bytes, past > 0 && past < self.size && (bytes >= self.ahead || self.budget.as_ref().is_some_and(|b| b.full())))
    }

    fn past_bytes(&self, head: Hour) -> usize {
        self.payload_between(self.built.partition_point(|step| step.last <= head), self.built.len())
    }

    fn payload_between(&self, start: usize, end: usize) -> usize {
        self.payload_offsets[end].wrapping_sub(self.payload_offsets[start])
    }

    fn furthest(&self) -> Option<u64> {
        self.consumers.iter().flatten().filter(|seat| seat.parked.is_none()).map(|seat| seat.next).max()
    }

    fn holds_from(&self, seat: &Seat) -> u64 {
        match seat.parked {
            None => seat.next,
            Some(from) => self.piece_end(from).map_or(self.next_seq, |step| step.seq),
        }
    }

    fn piece_end(&self, from: Hour) -> Option<&Arc<Step>> {
        self.built.iter().find(|step| step.first > from && (step.reset || step.piece.is_none()))
    }

    pub(crate) fn past(&self, head: Hour) -> usize {
        self.built.len() - self.built.partition_point(|step| step.last <= head)
    }

    #[cfg(test)]
    pub(crate) fn steps_kept(&self) -> usize {
        self.built.len()
    }

    #[cfg(test)]
    pub(crate) fn trailing_bytes(&self) -> usize {
        self.built.range(..self.trailing_end()).map(|step| step.payload()).sum()
    }

    pub(crate) fn set_bound(&mut self, bytes: usize) {
        self.bound = bytes;
    }

    #[cfg(test)]
    pub(crate) fn largest_kept(&self) -> usize {
        self.built.iter().map(|step| step.payload()).max().unwrap_or(0)
    }

    fn trailing_end(&self) -> usize {
        let furthest = self.consumers.iter().flatten().filter(|seat| seat.parked.is_none() && seat.own.is_empty()).map(|seat| seat.next).max();
        furthest.map_or(0, |next| next_of(&self.built, next))
    }

    fn park_behind(&mut self) {
        let end = self.trailing_end();
        if self.payload_between(0, end) <= self.bound {
            return;
        }
        let mut parks = Vec::new();
        for (id, seat) in self.consumers.iter().enumerate() {
            let Some(seat) = seat.as_ref().filter(|seat| seat.asks && !seat.lands_itself && seat.parked.is_none() && seat.own.is_empty() && seat.whole) else { continue };
            let at = next_of(&self.built, seat.next);
            let held = self.payload_between(at.min(end), end);
            if let Some(first) = self.built.get(at).filter(|step| held > self.bound && !step.reset) {
                parks.push((id, first.first - 1));
            }
        }
        let next = self.next_seq;
        for (id, from) in parks {
            if let Some(seat) = self.consumers[id].as_mut() {
                (seat.next, seat.parked) = (next, Some(from));
            }
        }
    }

    pub(crate) fn abandon(&mut self) -> u64 {
        self.epoch += 1;
        retire(&mut self.lent, self.budget.is_none(), self.built.drain(..));
        self.payload_offsets.truncate(1);
        (self.taken, self.reset) = (None, None);
        let next = self.next_seq;
        for seat in self.consumers.iter_mut().flatten() {
            seat.next = next;
            seat.parked = None;
            retire(&mut self.lent, self.budget.is_none(), seat.own.drain(..).map(|(_, step)| step));
        }
        retire(&mut self.lent, self.budget.is_none(), self.orphans.drain(..).map(|(_, _, step)| step));
        self.epoch
    }

    pub(crate) fn publish(&mut self, mut step: Arc<Step>) -> Result<(), Arc<Step>> {
        if step.epoch != self.epoch {
            return Err(step);
        }
        self.admit(&step);
        let held = Arc::get_mut(&mut step).expect("a step being published is held once");
        held.seq = self.next_seq;
        if held.reset {
            self.reset = Some((held.seq, held.last));
        }
        self.published = Some(held.last);
        self.published_in = self.epoch;
        self.next_seq += 1;
        self.payload_offsets.push_back(self.payload_offsets[self.built.len()].wrapping_add(step.payload()));
        self.built.push_back(step);
        Ok(())
    }

    pub(crate) fn reclaim(&mut self) -> (Option<Arc<Step>>, Vec<Arc<Step>>) {
        let (mut kept, mut spare, mut free) = (None, Vec::new(), 0);
        let mut at = 0;
        while at < self.lent.len() {
            if Arc::strong_count(&self.lent[at]) != 1 {
                at += 1;
                continue;
            }
            let oversized = self.lent[at].bytes_held() > 2 * self.lent[at].payload() + (1 << 20);
            if !oversized && kept.is_none() {
                kept = Some(self.lent.swap_remove(at));
            } else if !oversized && free < self.size {
                free += 1;
                at += 1;
            } else {
                spare.push(self.lent.swap_remove(at));
            }
        }
        (kept, spare)
    }

    pub(crate) fn settle(&mut self) -> Vec<Arc<Step>> {
        self.lent.extract_if(.., |step| Arc::strong_count(step) == 1).collect()
    }

    pub(crate) fn join(&mut self, reader: Reader) -> usize {
        let start = self.built.front().map_or(self.next_seq, |step| step.seq);
        let mut own = VecDeque::new();
        let mut at = 0;
        while at < self.orphans.len() {
            if self.orphans[at].0 == reader {
                let (_, before, step) = self.orphans.remove(at);
                own.push_back((before, step));
            } else {
                at += 1;
            }
        }
        self.consumers.push(Some(Seat { reader, next: start, own, parked: None, whole: true, asks: false, lands_itself: false }));
        self.consumers.len() - 1
    }

    pub(crate) fn leave(&mut self, id: usize) {
        if let Some(seat) = self.consumers[id].take() {
            retire(&mut self.lent, self.budget.is_none(), seat.own.into_iter().map(|(_, step)| step));
        }
        self.trim();
    }

    pub(crate) fn forget(&mut self, reader: Reader) {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.orphans).into_iter().partition(|(held, _, _)| *held == reader);
        self.orphans = kept;
        retire(&mut self.lent, self.budget.is_none(), gone.into_iter().map(|(_, _, step)| step));
    }

    pub(crate) fn lands_itself(&mut self, id: usize) {
        if let Some(seat) = self.consumers.get_mut(id).and_then(Option::as_mut) {
            seat.lands_itself = true;
        }
    }

    pub(crate) fn asks(&mut self, id: usize) {
        if let Some(seat) = self.consumers.get_mut(id).and_then(Option::as_mut) {
            seat.asks = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn asked(&self, id: usize) -> bool {
        self.consumers.get(id).and_then(Option::as_ref).is_some_and(|seat| seat.asks)
    }

    pub(crate) fn reader_of(&self, id: usize) -> Option<Reader> {
        self.consumers.get(id)?.as_ref().map(|seat| seat.reader)
    }

    pub(crate) fn stands_at(&self, id: usize) -> Option<Hour> {
        let seat = self.consumers.get(id)?.as_ref().filter(|seat| seat.parked.is_none() && seat.whole)?;
        match self.built.get(next_of(&self.built, seat.next)) {
            Some(step) => (!step.reset).then(|| step.first - 1),
            None => self.published,
        }
    }

    pub(crate) fn holding(&self, step: &Arc<Step>) -> Vec<usize> {
        self.consumers.iter().enumerate().filter(|(_, seat)| seat.as_ref().is_some_and(|seat| seat.own.iter().any(|(_, held)| Arc::ptr_eq(held, step)))).map(|(id, _)| id).collect()
    }

    pub(crate) fn land_standing(&mut self, id: usize, hour: Hour, step: Arc<Step>, revoke: &Arc<Step>) -> Result<(), Arc<Step>> {
        if self.stands_at(id) != Some(hour) {
            return Err(step);
        }
        self.admit(&step);
        let Some(seat) = self.consumers[id].as_mut() else { return Err(step) };
        let Some(at) = seat.own.iter().position(|(_, held)| Arc::ptr_eq(held, revoke)) else { return Err(step) };
        retire(&mut self.lent, self.budget.is_none(), seat.own.remove(at).map(|(_, held)| held));
        seat.own.push_front((seat.next, step));
        Ok(())
    }

    pub(crate) fn published_now(&self) -> Option<(Hour, u64)> {
        self.published.filter(|_| self.published_in == self.epoch).map(|hour| (hour, self.epoch))
    }

    pub(crate) fn parked(&self, id: usize) -> Option<Hour> {
        self.consumers.get(id)?.as_ref()?.parked
    }

    pub(crate) fn parked_seats(&self) -> Vec<(usize, Reader, Hour)> {
        self.consumers.iter().enumerate().filter_map(|(id, seat)| seat.as_ref().and_then(|seat| Some((id, seat.reader, seat.parked?)))).collect()
    }

    pub(crate) fn catch_hour(&self, id: usize) -> Option<Hour> {
        let from = self.parked(id)?;
        let hour = match self.furthest() {
            Some(next) => self.built.get(next_of(&self.built, next)).map(|step| step.first - 1).or(self.published)?,
            None => self.published?,
        };
        Some(self.piece_end(from).map_or(hour, |step| hour.min(step.first - 1)))
    }

    pub(crate) fn land_at(&mut self, id: usize, hour: Hour, steps: Vec<Arc<Step>>) -> Result<(), Vec<Arc<Step>>> {
        let position = match self.built.iter().find(|step| step.last > hour) {
            Some(step) if step.first == hour + 1 => step.seq,
            None if self.published == Some(hour) => self.next_seq,
            _ => return Err(steps),
        };
        for step in &steps { self.admit(step); }
        let Some(seat) = self.consumers.get_mut(id).and_then(Option::as_mut).filter(|seat| seat.parked.is_some()) else { return Err(steps) };
        (seat.next, seat.parked) = (position, None);
        seat.own.extend(steps.into_iter().map(|step| (position, step)));
        self.park_behind();
        self.trim();
        Ok(())
    }

    pub(crate) fn take(&mut self, id: usize, head: Hour, out: &mut Vec<Arc<Step>>) -> usize {
        let Some(seat) = self.consumers[id].as_mut().filter(|seat| seat.parked.is_none()) else { return 0 };
        let mut taken = 0;
        let mut at = next_of(&self.built, seat.next);
        loop {
            if let Some((before, step)) = seat.own.front()
                && *before <= seat.next
            {
                if step.last > head {
                    break;
                }
                seat.whole = whole(step);
                out.push(Arc::clone(step));
                retire(&mut self.lent, self.budget.is_none(), [seat.own.pop_front().expect("held").1]);
                taken += 1;
                continue;
            }
            let Some(step) = self.built.get(at) else { break };
            if step.last > head {
                break;
            }
            self.taken = self.taken.max(Some(step.last));
            seat.whole = whole(step);
            out.push(Arc::clone(step));
            seat.next = step.seq + 1;
            at += 1;
            taken += 1;
        }
        if taken > 0 {
            self.park_behind();
            self.trim();
        }
        taken
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn has_ready(&self, id: usize, head: Hour) -> bool {
        self.reaches(id, head)
    }

    pub(crate) fn ahead(&self, id: usize, out: &mut Vec<Arc<Step>>) {
        let Some(seat) = self.consumers[id].as_ref().filter(|seat| seat.parked.is_none()) else { return };
        let mut next = seat.next;
        let mut own = seat.own.iter().peekable();
        for step in self.built.range(next_of(&self.built, next)..) {
            while let Some((_, held)) = own.next_if(|(before, _)| *before <= next) {
                out.push(Arc::clone(held));
            }
            out.push(Arc::clone(step));
            next = step.seq + 1;
        }
        out.extend(own.map(|(_, held)| Arc::clone(held)));
    }

    pub(crate) fn reaches(&self, id: usize, head: Hour) -> bool {
        self.next_hour(id).is_some_and(|next| next <= head)
    }

    pub(crate) fn next_hour(&self, id: usize) -> Option<Hour> {
        let seat = self.consumers[id].as_ref().filter(|seat| seat.parked.is_none())?;
        match seat.own.front().filter(|(before, _)| *before <= seat.next) {
            Some((_, step)) => Some(step.last),
            None => self.built.get(next_of(&self.built, seat.next)).map(|step| step.last),
        }
    }

    pub(crate) fn frames(&self) -> Option<(u64, Hour)> {
        let (reset, hour) = self.reset?;
        let position = self.furthest().unwrap_or(0).max(reset + 1);
        if position == reset + 1 {
            return Some((position, hour));
        }
        let before = next_of(&self.built, position).checked_sub(1).and_then(|at| self.built.get(at)).filter(|step| step.seq + 1 == position);
        Some((position, before.map(|step| step.last).or(self.taken).unwrap_or(hour)))
    }

    pub(crate) fn plan(&self, epoch: u64) -> Option<(u64, Hour, Vec<Span>)> {
        if epoch != self.epoch {
            return None;
        }
        let (position, hour) = self.frames()?;
        let past = self.built.range(next_of(&self.built, position)..);
        if past.clone().any(|step| step.reset) {
            return None;
        }
        Some((position, hour, past.map(|step| Span { seq: step.seq, first: step.first, last: step.last, gap: step.piece.is_none() }).collect()))
    }

    pub(crate) fn complete(&mut self, sets: Vec<(u64, Vec<Changes>)>) -> Vec<Changes> {
        let mut reservations = VecDeque::new();
        if let Some(budget) = &self.budget {
            for (seq, set) in &sets {
                let Ok(at) = self.built.binary_search_by_key(seq, |s| s.seq) else { continue };
                let step = &self.built[at];
                let before = step.payload();
                let mut after = before;
                for changes in set {
                    after -= step.table(changes.table()).map_or(0, Changes::payload);
                    after += changes.payload();
                }
                let mut reserved = budget.charge();
                reserved.resize(if Arc::strong_count(step) == 1 { after.saturating_sub(before) } else { after });
                reservations.push_back((reserved, after));
            }
        }
        let mut replaced = Vec::new();
        let mut first_changed = self.built.len();
        for (seq, set) in sets {
            let Ok(at) = self.built.binary_search_by_key(&seq, |step| step.seq) else {
                replaced.extend(set);
                continue;
            };
            first_changed = first_changed.min(at);
            let step = &mut self.built[at];
            if Arc::get_mut(step).is_none() {
                *step = Arc::new(step.copied());
            }
            let step = Arc::get_mut(step).expect("a step no consumer took is held by the ring alone once copied");
            if let Some((reserved, after)) = reservations.pop_front() {
                let charge = step.charge.get_mut().unwrap();
                if let Some(existing) = charge {
                    existing.absorb(reserved);
                    existing.resize(after);
                } else { *charge = Some(reserved); }
            }
            for changes in set {
                match step.tables.binary_search_by_key(&changes.table(), Changes::table) {
                    Ok(place) => replaced.push(std::mem::replace(&mut step.tables[place], changes)),
                    Err(place) => step.tables.insert(place, changes),
                }
            }
        }
        for at in first_changed..self.built.len() {
            self.payload_offsets[at + 1] = self.payload_offsets[at].wrapping_add(self.built[at].payload());
        }
        replaced
    }

    pub(crate) fn give(&mut self, reader: Reader, before: u64, steps: &[Arc<Step>]) {
        for step in steps { self.admit(step); }
        let mut given = false;
        for seat in self.consumers.iter_mut().flatten().filter(|seat| seat.reader == reader) {
            let at = place(&seat.own, before);
            for (offset, step) in steps.iter().enumerate() {
                seat.own.insert(at + offset, (before, Arc::clone(step)));
            }
            given = true;
        }
        if !given {
            self.orphans.extend(steps.iter().map(|step| (reader, before, Arc::clone(step))));
        }
    }

    pub(crate) fn holds_own(&self, step: &Arc<Step>) -> bool {
        self.consumers.iter().flatten().any(|seat| seat.own.iter().any(|(_, held)| Arc::ptr_eq(held, step))) || self.orphans.iter().any(|(_, _, held)| Arc::ptr_eq(held, step))
    }

    pub(crate) fn jump(&mut self, id: usize, step: Arc<Step>) {
        self.admit(&step);
        let next = self.next_seq;
        let Some(seat) = self.consumers[id].as_mut() else {
            retire(&mut self.lent, self.budget.is_none(), [step]);
            return;
        };
        (seat.next, seat.parked) = (next, None);
        retire(&mut self.lent, self.budget.is_none(), seat.own.drain(..).map(|(_, held)| held));
        seat.own.push_back((next, step));
        self.park_behind();
        self.trim();
    }

    pub(crate) fn cut(&mut self, position: u64) -> (Vec<(Reader, Vec<usize>)>, Vec<usize>) {
        let at = next_of(&self.built, position);
        retire(&mut self.lent, self.budget.is_none(), self.built.drain(at..));
        self.payload_offsets.truncate(at + 1);
        let (mut owed, mut landing) = (Vec::new(), Vec::new());
        let tables = |step: &Step| step.tables.iter().map(Changes::table).collect::<Vec<_>>();
        for (id, seat) in self.consumers.iter_mut().enumerate() {
            let Some(seat) = seat.as_mut().filter(|seat| seat.parked.is_none()) else { continue };
            if seat.next > position {
                seat.next = position;
                landing.push(id);
                retire(&mut self.lent, self.budget.is_none(), seat.own.drain(..).map(|(_, step)| step));
                continue;
            }
            let kept = place(&seat.own, position);
            for (_, step) in seat.own.drain(kept..) {
                owed.push((seat.reader, tables(&step)));
                retire(&mut self.lent, self.budget.is_none(), [step]);
            }
        }
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.orphans).into_iter().partition(|(_, before, _)| *before > position);
        self.orphans = kept;
        for (reader, _, step) in gone {
            owed.push((reader, tables(&step)));
            retire(&mut self.lent, self.budget.is_none(), [step]);
        }
        (owed, landing)
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    fn trim(&mut self) {
        let Some(least) = self.consumers.iter().flatten().map(|seat| self.holds_from(seat)).min() else { return };
        while self.built.front().is_some_and(|step| step.seq < least) {
            retire(&mut self.lent, self.budget.is_none(), [self.built.pop_front().expect("held")]);
            self.payload_offsets.pop_front();
        }
    }

    pub(crate) fn steps(&self) -> Vec<Arc<Step>> {
        let own = self.consumers.iter().flatten().flat_map(|seat| seat.own.iter().map(|(_, step)| step));
        self.built.iter().chain(self.lent.iter()).chain(own).chain(self.orphans.iter().map(|(_, _, step)| step)).cloned().collect()
    }
}
