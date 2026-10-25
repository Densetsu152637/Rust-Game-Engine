//! Rule-based matchmaking core (sans-IO, deterministic).
//!
//! Each [`ModeRules`] defines teams, a skill window that widens with wait time, a latency bound
//! for region compatibility and attribute keys that must be equal. [`Matchmaker::tick`] walks the
//! tickets of every mode oldest-first; each unmatched ticket in turn is the *anchor* of a match
//! attempt. Candidates inside the anchor's current skill window are added closest-rating-first
//! while a common region survives and the party sizes still pack into teams. A match forms only
//! when the teams are exactly full. Iteration order is fully determined by `(enqueue time,
//! ticket id)` so the same inputs always give the same matches.
//!
//! A ticket's *local party* (`party_size`) are players sharing one connection (split-screen
//! guests); remote parties are lobby sessions. The rating is a claim unless the service overrides
//! it from a [`crate::PlayerDirectory`].
use crate::{
    limits::OnlineLimits,
    protocol::{Attributes, Hosting, TicketId, TicketSpec},
};
use engine_network::PeerId;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Matching rules for one game mode.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModeRules {
    pub name: String,
    pub team_count: u8,
    pub team_size: u8,
    /// Skill window (rating points) at zero wait.
    pub base_window: u32,
    /// Window growth per full second waited.
    pub widen_per_second: u32,
    pub max_window: u32,
    /// A region is acceptable to a ticket when its measured latency is at most this.
    pub max_latency_ms: u16,
    /// Attribute keys whose values must be equal across a match (missing counts as empty).
    pub must_match: Vec<String>,
    pub hosting: Hosting,
    /// Tickets older than this expire unmatched.
    pub max_wait_ms: u64,
}
impl Default for ModeRules {
    fn default() -> Self {
        Self {
            name: "duel".into(),
            team_count: 2,
            team_size: 1,
            base_window: 50,
            widen_per_second: 25,
            max_window: 1000,
            max_latency_ms: 120,
            must_match: Vec::new(),
            hosting: Hosting::Player,
            max_wait_ms: 120_000,
        }
    }
}
impl ModeRules {
    pub fn players_per_match(&self) -> usize {
        usize::from(self.team_count) * usize::from(self.team_size)
    }
    fn validate(&self) -> bool {
        crate::text::valid_ident(&self.name, 32)
            && self.team_count >= 1
            && self.team_size >= 1
            && self.players_per_match() <= 64
            && self.max_window >= self.base_window
            && self.max_wait_ms > 0
            && self.must_match.len() <= 8
            && self
                .must_match
                .iter()
                .all(|k| crate::text::valid_ident(k, 24))
    }
    /// Skill window after `wait_ms`: `base + widen * whole seconds`, capped at `max_window`.
    pub fn window(&self, wait_ms: u64) -> u32 {
        let grown = u64::from(self.base_window)
            .saturating_add(u64::from(self.widen_per_second).saturating_mul(wait_ms / 1000));
        grown.min(u64::from(self.max_window)) as u32
    }
}

/// Rules of the reference server when the config lists none.
pub fn default_modes() -> Vec<ModeRules> {
    vec![
        ModeRules::default(),
        ModeRules {
            name: "coop4".into(),
            team_count: 1,
            team_size: 4,
            base_window: 200,
            widen_per_second: 50,
            max_window: 2000,
            ..ModeRules::default()
        },
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub id: TicketId,
    pub peer: PeerId,
    pub subject: String,
    pub mode: String,
    pub region: String,
    pub rating: i32,
    pub party_size: u8,
    pub attributes: Attributes,
    pub latencies: BTreeMap<String, u16>,
    pub candidates: Vec<u8>,
    pub rtt_ms: Option<u32>,
    pub enqueued_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchError {
    UnknownMode,
    QueueFull,
    AlreadyQueued,
    Invalid(&'static str),
}

/// A formed match: `teams[i]` are the tickets of team `i`.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchResult {
    pub mode: String,
    /// Smallest region acceptable to every ticket.
    pub region: String,
    pub hosting: Hosting,
    pub teams: Vec<Vec<Ticket>>,
    /// Highest minus lowest rating among the tickets.
    pub rating_spread: i32,
    /// Longest wait among the tickets at match time.
    pub max_wait_ms: u64,
}
impl MatchResult {
    pub fn tickets(&self) -> impl Iterator<Item = &Ticket> {
        self.teams.iter().flatten()
    }
}

#[derive(Debug, Default)]
pub struct TickOutput {
    pub matches: Vec<MatchResult>,
    pub expired: Vec<Ticket>,
}

pub struct Matchmaker {
    limits: OnlineLimits,
    rules: BTreeMap<String, ModeRules>,
    tickets: BTreeMap<TicketId, Ticket>,
    by_peer: BTreeMap<PeerId, TicketId>,
    next_id: TicketId,
}
impl Matchmaker {
    pub fn new(limits: OnlineLimits, modes: Vec<ModeRules>) -> Result<Self, MatchError> {
        let mut rules = BTreeMap::new();
        for mode in modes {
            if !mode.validate()
                || mode.players_per_match() > usize::from(limits.max_capacity)
                || rules.insert(mode.name.clone(), mode).is_some()
            {
                return Err(MatchError::Invalid("invalid or duplicate mode rules"));
            }
        }
        if rules.is_empty() {
            return Err(MatchError::Invalid("no modes configured"));
        }
        Ok(Self {
            limits,
            rules,
            tickets: BTreeMap::new(),
            by_peer: BTreeMap::new(),
            next_id: 1,
        })
    }
    pub fn len(&self) -> usize {
        self.tickets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }
    pub fn modes(&self) -> impl Iterator<Item = &ModeRules> {
        self.rules.values()
    }
    pub fn ticket_of(&self, peer: PeerId) -> Option<TicketId> {
        self.by_peer.get(&peer).copied()
    }

    /// Queues a ticket for `peer` (one per peer). The spec must already have passed protocol
    /// validation; the mode and party size are checked against the rules here.
    pub fn enqueue(
        &mut self,
        peer: PeerId,
        subject: &str,
        spec: &TicketSpec,
        rtt_ms: Option<u32>,
        now_ms: u64,
    ) -> Result<TicketId, MatchError> {
        let rules = self.rules.get(&spec.mode).ok_or(MatchError::UnknownMode)?;
        if spec.party_size == 0 || spec.party_size > rules.team_size {
            return Err(MatchError::Invalid("party larger than a team"));
        }
        if self.by_peer.contains_key(&peer) {
            return Err(MatchError::AlreadyQueued);
        }
        let in_mode = self
            .tickets
            .values()
            .filter(|t| t.mode == spec.mode)
            .count();
        if self.tickets.len() >= self.limits.max_tickets
            || in_mode >= self.limits.max_tickets_per_mode
        {
            return Err(MatchError::QueueFull);
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(MatchError::QueueFull)?;
        self.tickets.insert(
            id,
            Ticket {
                id,
                peer,
                subject: subject.to_owned(),
                mode: spec.mode.clone(),
                region: spec.region.clone(),
                rating: spec.rating,
                party_size: spec.party_size,
                attributes: spec.attributes.clone(),
                latencies: spec.latencies.clone(),
                candidates: spec
                    .candidates
                    .as_ref()
                    .map(|b| b.0.clone())
                    .unwrap_or_default(),
                rtt_ms,
                enqueued_ms: now_ms,
            },
        );
        self.by_peer.insert(peer, id);
        Ok(id)
    }

    /// Cancels `ticket` if it belongs to `peer`.
    pub fn cancel(&mut self, peer: PeerId, ticket: TicketId) -> bool {
        if self.by_peer.get(&peer) != Some(&ticket) {
            return false;
        }
        self.remove(ticket).is_some()
    }
    /// Drops the peer's ticket (disconnect).
    pub fn peer_gone(&mut self, peer: PeerId) -> Option<Ticket> {
        let id = self.by_peer.get(&peer).copied()?;
        self.remove(id)
    }
    fn remove(&mut self, id: TicketId) -> Option<Ticket> {
        let ticket = self.tickets.remove(&id)?;
        self.by_peer.remove(&ticket.peer);
        Some(ticket)
    }

    /// One deterministic matching pass at `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> TickOutput {
        let mut output = TickOutput::default();
        let names: Vec<String> = self.rules.keys().cloned().collect();
        for name in names {
            let rules = self.rules[&name].clone();
            for group in self.form_matches(&rules, now_ms) {
                let tickets: Vec<Ticket> = group.iter().filter_map(|id| self.remove(*id)).collect();
                output.matches.push(build_result(&rules, tickets, now_ms));
            }
            let expired: Vec<TicketId> = self
                .tickets
                .values()
                .filter(|t| {
                    t.mode == name && now_ms.saturating_sub(t.enqueued_ms) >= rules.max_wait_ms
                })
                .map(|t| t.id)
                .collect();
            output
                .expired
                .extend(expired.into_iter().filter_map(|id| self.remove(id)));
        }
        output
    }

    /// Ticket IDs of each match that can form now, in formation order.
    fn form_matches(&self, rules: &ModeRules, now_ms: u64) -> Vec<Vec<TicketId>> {
        let mut pool: Vec<&Ticket> = self
            .tickets
            .values()
            .filter(|t| t.mode == rules.name)
            .collect();
        pool.sort_by_key(|t| (t.enqueued_ms, t.id));
        let regions: Vec<BTreeSet<&str>> =
            pool.iter().map(|t| acceptable_regions(t, rules)).collect();
        let mut used = vec![false; pool.len()];
        let mut formed = Vec::new();
        let mut anchors = 0;
        for anchor in 0..pool.len() {
            if used[anchor] {
                continue;
            }
            if anchors >= self.limits.max_anchors_per_tick {
                break;
            }
            anchors += 1;
            if let Some(members) = try_form(rules, &pool, &regions, &used, anchor, now_ms) {
                for &index in &members {
                    used[index] = true;
                }
                formed.push(members.iter().map(|&i| pool[i].id).collect());
            }
        }
        formed
    }
}

fn acceptable_regions<'a>(ticket: &'a Ticket, rules: &ModeRules) -> BTreeSet<&'a str> {
    let mut set: BTreeSet<&str> = ticket
        .latencies
        .iter()
        .filter(|&(_, &ms)| ms <= rules.max_latency_ms)
        .map(|(region, _)| region.as_str())
        .collect();
    set.insert(ticket.region.as_str());
    set
}

fn attributes_match(rules: &ModeRules, a: &Ticket, b: &Ticket) -> bool {
    rules.must_match.iter().all(|key| {
        a.attributes.get(key).map_or("", String::as_str)
            == b.attributes.get(key).map_or("", String::as_str)
    })
}

/// Places party sizes into `team_count` teams of `team_size`, balancing rating sums. Returns the
/// team index per input position, or `None` if the greedy packing fails.
fn assign_teams(rules: &ModeRules, chosen: &[&Ticket]) -> Option<Vec<usize>> {
    let mut order: Vec<usize> = (0..chosen.len()).collect();
    order.sort_by_key(|&i| {
        (
            std::cmp::Reverse(chosen[i].party_size),
            std::cmp::Reverse(chosen[i].rating),
            chosen[i].id,
        )
    });
    let size = usize::from(rules.team_size);
    let mut load = vec![(0usize, 0i64); usize::from(rules.team_count)];
    let mut team_of = vec![0usize; chosen.len()];
    for i in order {
        let party = usize::from(chosen[i].party_size);
        let (team, _) = load
            .iter()
            .enumerate()
            .filter(|(_, (count, _))| count + party <= size)
            .min_by_key(|(index, (_, sum))| (*sum, *index))?;
        load[team].0 += party;
        load[team].1 += i64::from(chosen[i].rating) * party as i64;
        team_of[i] = team;
    }
    Some(team_of)
}

/// Tries to complete a match around `pool[anchor]`; returns pool indices on success.
fn try_form(
    rules: &ModeRules,
    pool: &[&Ticket],
    regions: &[BTreeSet<&str>],
    used: &[bool],
    anchor: usize,
    now_ms: u64,
) -> Option<Vec<usize>> {
    let need = rules.players_per_match();
    let anchor_ticket = pool[anchor];
    let window = i64::from(rules.window(now_ms.saturating_sub(anchor_ticket.enqueued_ms)));
    let mut common: BTreeSet<&str> = regions[anchor].clone();
    let mut candidates: Vec<usize> = (0..pool.len())
        .filter(|&i| {
            i != anchor
                && !used[i]
                && i64::from(pool[i].rating).abs_diff(i64::from(anchor_ticket.rating)) as i64
                    <= window
                && attributes_match(rules, anchor_ticket, pool[i])
                && !regions[i].is_disjoint(&regions[anchor])
        })
        .collect();
    candidates.sort_by_key(|&i| {
        (
            i64::from(pool[i].rating).abs_diff(i64::from(anchor_ticket.rating)),
            pool[i].enqueued_ms,
            pool[i].id,
        )
    });
    let mut members = vec![anchor];
    let mut total = usize::from(anchor_ticket.party_size);
    for i in candidates {
        if total == need {
            break;
        }
        let party = usize::from(pool[i].party_size);
        if total + party > need {
            continue;
        }
        let intersection: BTreeSet<&str> = common.intersection(&regions[i]).copied().collect();
        if intersection.is_empty() {
            continue;
        }
        members.push(i);
        let chosen: Vec<&Ticket> = members.iter().map(|&m| pool[m]).collect();
        if assign_teams(rules, &chosen).is_none() {
            members.pop();
            continue;
        }
        common = intersection;
        total += party;
    }
    (total == need).then_some(members)
}

fn build_result(rules: &ModeRules, tickets: Vec<Ticket>, now_ms: u64) -> MatchResult {
    let refs: Vec<&Ticket> = tickets.iter().collect();
    // Region: smallest acceptable to everyone (non-empty by construction).
    let mut common: Option<BTreeSet<&str>> = None;
    for t in &refs {
        let set = acceptable_regions(t, rules);
        common = Some(match common {
            None => set,
            Some(c) => c.intersection(&set).copied().collect(),
        });
    }
    let region = common
        .and_then(|c| c.first().map(|r| (*r).to_owned()))
        .unwrap_or_default();
    let assignment = assign_teams(rules, &refs).unwrap_or_else(|| vec![0; refs.len()]);
    let min = tickets.iter().map(|t| t.rating).min().unwrap_or(0);
    let max = tickets.iter().map(|t| t.rating).max().unwrap_or(0);
    let max_wait_ms = tickets
        .iter()
        .map(|t| now_ms.saturating_sub(t.enqueued_ms))
        .max()
        .unwrap_or(0);
    let mut teams = vec![Vec::new(); usize::from(rules.team_count)];
    let mut ordered: Vec<(usize, Ticket)> = assignment.into_iter().zip(tickets).collect();
    ordered.sort_by_key(|(team, t)| (*team, t.enqueued_ms, t.id));
    for (team, ticket) in ordered {
        teams[team].push(ticket);
    }
    MatchResult {
        mode: rules.name.clone(),
        region,
        hosting: rules.hosting,
        teams,
        rating_spread: max - min,
        max_wait_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(mode: &str, region: &str, rating: i32, party: u8) -> TicketSpec {
        TicketSpec {
            mode: mode.into(),
            region: region.into(),
            rating,
            party_size: party,
            attributes: Attributes::new(),
            latencies: BTreeMap::new(),
            candidates: None,
        }
    }
    fn mm(rules: Vec<ModeRules>) -> Matchmaker {
        Matchmaker::new(OnlineLimits::default(), rules).unwrap()
    }
    fn duel() -> ModeRules {
        ModeRules::default()
    }

    #[test]
    fn window_widens_with_wait_and_is_capped() {
        let r = duel();
        assert_eq!(r.window(0), 50);
        assert_eq!(r.window(999), 50);
        assert_eq!(r.window(1000), 75);
        assert_eq!(r.window(10_000), 300);
        assert_eq!(r.window(10_000_000), 1000);
        assert_eq!(r.window(u64::MAX), 1000);
    }

    #[test]
    fn close_ratings_match_immediately_distant_ones_wait_then_match() {
        let mut m = mm(vec![duel()]);
        m.enqueue(1, "a", &spec("duel", "eu", 1500, 1), None, 0)
            .unwrap();
        m.enqueue(2, "b", &spec("duel", "eu", 1540, 1), None, 0)
            .unwrap();
        m.enqueue(3, "c", &spec("duel", "eu", 1900, 1), None, 0)
            .unwrap();
        let out = m.tick(0);
        assert_eq!(out.matches.len(), 1);
        let mut ids: Vec<PeerId> = out.matches[0].tickets().map(|t| t.peer).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(out.matches[0].rating_spread, 40);
        assert_eq!(out.matches[0].region, "eu");
        // The outlier waits; peer 4 arrives within the outlier's widened window.
        m.enqueue(4, "d", &spec("duel", "eu", 1650, 1), None, 5_000)
            .unwrap();
        assert!(
            m.tick(5_000).matches.is_empty(),
            "|1900-1650| > window(5s)=175"
        );
        let out = m.tick(10_000);
        assert_eq!(out.matches.len(), 1, "window(10s)=300 now covers 250");
        assert_eq!(out.matches[0].rating_spread, 250);
        assert!(m.is_empty());
    }

    #[test]
    fn tickets_expire_cancel_and_queues_are_bounded() {
        let limits = OnlineLimits {
            max_tickets: 3,
            max_tickets_per_mode: 2,
            ..OnlineLimits::default()
        };
        let mut m = Matchmaker::new(
            limits,
            vec![
                ModeRules {
                    max_wait_ms: 3000,
                    ..duel()
                },
                ModeRules {
                    name: "ffa".into(),
                    ..duel()
                },
            ],
        )
        .unwrap();
        assert_eq!(
            m.enqueue(1, "a", &spec("nope", "eu", 1, 1), None, 0),
            Err(MatchError::UnknownMode)
        );
        assert_eq!(
            m.enqueue(1, "a", &spec("duel", "eu", 1, 2), None, 0),
            Err(MatchError::Invalid("party larger than a team"))
        );
        let a = m
            .enqueue(1, "a", &spec("duel", "eu", 0, 1), None, 0)
            .unwrap();
        assert_eq!(
            m.enqueue(1, "a", &spec("duel", "eu", 0, 1), None, 0),
            Err(MatchError::AlreadyQueued)
        );
        m.enqueue(2, "b", &spec("duel", "us", 9999, 1), None, 0)
            .unwrap();
        assert_eq!(
            m.enqueue(3, "c", &spec("duel", "eu", 0, 1), None, 0),
            Err(MatchError::QueueFull),
            "per-mode bound"
        );
        m.enqueue(4, "d", &spec("ffa", "eu", 0, 1), None, 0)
            .unwrap();
        assert_eq!(
            m.enqueue(5, "e", &spec("ffa", "eu", 0, 1), None, 0),
            Err(MatchError::QueueFull),
            "global bound"
        );
        assert!(!m.cancel(2, a), "only the owner can cancel");
        assert!(m.cancel(1, a));
        assert!(!m.cancel(1, a));
        let out = m.tick(3000);
        assert_eq!(
            out.expired.iter().map(|t| t.peer).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(m.peer_gone(4).is_some());
        assert!(m.is_empty());
    }

    #[test]
    fn region_rule_uses_shared_regions_and_latency_sets() {
        let mut m = mm(vec![duel()]);
        let mut near = spec("duel", "eu", 1500, 1);
        near.latencies.insert("us".into(), 150);
        m.enqueue(1, "a", &near, None, 0).unwrap();
        m.enqueue(2, "b", &spec("duel", "us", 1500, 1), None, 0)
            .unwrap();
        assert!(m.tick(0).matches.is_empty(), "us at 150ms exceeds 120ms");
        let mut ok = spec("duel", "eu", 1500, 1);
        ok.latencies.insert("us".into(), 90);
        m.enqueue(3, "c", &ok, None, 0).unwrap();
        let out = m.tick(0);
        assert_eq!(out.matches.len(), 1);
        // Peer 3 (eu + us@90) pairs with the oldest compatible ticket; result region is shared.
        let region = &out.matches[0].region;
        assert!(["eu", "us"].contains(&region.as_str()));
        let peers: BTreeSet<PeerId> = out.matches[0].tickets().map(|t| t.peer).collect();
        assert!(peers.contains(&3));
    }

    #[test]
    fn must_match_attributes_partition_the_pool() {
        let mut m = mm(vec![ModeRules {
            must_match: vec!["map".into()],
            ..duel()
        }]);
        let with = |map: &str| {
            let mut s = spec("duel", "eu", 1500, 1);
            s.attributes.insert("map".into(), map.into());
            s
        };
        m.enqueue(1, "a", &with("dust"), None, 0).unwrap();
        m.enqueue(2, "b", &with("ice"), None, 0).unwrap();
        assert!(m.tick(0).matches.is_empty());
        m.enqueue(3, "c", &with("ice"), None, 0).unwrap();
        let out = m.tick(0);
        let mut peers: Vec<PeerId> = out.matches[0].tickets().map(|t| t.peer).collect();
        peers.sort();
        assert_eq!(peers, vec![2, 3]);
    }

    #[test]
    fn parties_that_fit_form_exactly_full_teams() {
        let rules = ModeRules {
            name: "squad".into(),
            team_count: 2,
            team_size: 3,
            base_window: 1000,
            ..duel()
        };
        let mut m = mm(vec![rules]);
        for (peer, party, rating) in [(1, 2, 1500), (2, 2, 1490), (3, 1, 1520), (4, 1, 1480)] {
            m.enqueue(peer, "p", &spec("squad", "eu", rating, party), None, 0)
                .unwrap();
        }
        let out = m.tick(0);
        assert_eq!(out.matches.len(), 1);
        for team in &out.matches[0].teams {
            assert_eq!(
                team.iter()
                    .map(|t| usize::from(t.party_size))
                    .sum::<usize>(),
                3
            );
        }
    }

    #[test]
    fn modes_larger_than_a_session_or_duplicated_are_rejected() {
        let huge = ModeRules {
            name: "huge".into(),
            team_count: 2,
            team_size: 20,
            ..duel()
        };
        assert!(Matchmaker::new(OnlineLimits::default(), vec![huge]).is_err());
        assert!(Matchmaker::new(OnlineLimits::default(), vec![duel(), duel()]).is_err());
        assert!(Matchmaker::new(OnlineLimits::default(), vec![]).is_err());
        let bad_window = ModeRules {
            base_window: 500,
            max_window: 100,
            ..duel()
        };
        assert!(Matchmaker::new(OnlineLimits::default(), vec![bad_window]).is_err());
    }

    #[test]
    fn tick_is_deterministic() {
        let run = || {
            let mut m = mm(vec![duel()]);
            let mut state = 12345u64;
            let mut rng = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let mut log = Vec::new();
            for step in 0..200u64 {
                for k in 0..3 {
                    let peer = step * 10 + k;
                    let rating = (rng() % 3000) as i32;
                    let _ = m.enqueue(peer, "p", &spec("duel", "eu", rating, 1), None, step * 1000);
                }
                for found in m.tick(step * 1000).matches {
                    log.push(found.tickets().map(|t| t.id).collect::<Vec<_>>());
                }
            }
            log
        };
        assert_eq!(run(), run());
        assert!(!run().is_empty());
    }

    /// Seeded population simulation: match quality (rating spread) against wait time, with and
    /// without skill-window widening. Numbers are printed (`--nocapture`).
    #[test]
    fn match_quality_versus_wait_time_over_a_seeded_population() {
        struct Outcome {
            matched_players: usize,
            expired: usize,
            spreads: Vec<(u64, i32)>,
        }
        fn simulate(widen: u32, seed: u64) -> Outcome {
            let rules = ModeRules {
                widen_per_second: widen,
                max_window: 1000,
                max_wait_ms: 60_000,
                ..duel()
            };
            let mut m = Matchmaker::new(
                OnlineLimits {
                    max_tickets: 100_000,
                    max_tickets_per_mode: 100_000,
                    ..OnlineLimits::default()
                },
                vec![rules],
            )
            .unwrap();
            let mut state = seed;
            let mut rng = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let mut outcome = Outcome {
                matched_players: 0,
                expired: 0,
                spreads: Vec::new(),
            };
            let mut peer = 0;
            for second in 0..900u64 {
                if second < 600 {
                    // 3 arrivals per second; rating ~ N(1500, ~283) as a sum of six uniforms.
                    for _ in 0..3 {
                        let sum: i64 = (0..6).map(|_| (rng() % 2001) as i64 - 1000).sum();
                        let rating = (1500 + sum / 5).clamp(0, 3000) as i32;
                        peer += 1;
                        m.enqueue(
                            peer,
                            "p",
                            &spec("duel", "eu", rating, 1),
                            None,
                            second * 1000,
                        )
                        .unwrap();
                    }
                }
                let out = m.tick(second * 1000);
                for found in out.matches {
                    outcome.matched_players += found.tickets().count();
                    outcome
                        .spreads
                        .push((found.max_wait_ms, found.rating_spread));
                }
                outcome.expired += out.expired.len();
            }
            outcome
        }
        let report = |label: &str, o: &Outcome| {
            let buckets: [(u64, u64); 5] = [
                (0, 1000),
                (1000, 5000),
                (5000, 10_000),
                (10_000, 20_000),
                (20_000, u64::MAX),
            ];
            println!(
                "{label}: matched_players={} expired={}",
                o.matched_players, o.expired
            );
            let mut means = Vec::new();
            for (lo, hi) in buckets {
                let in_bucket: Vec<i32> = o
                    .spreads
                    .iter()
                    .filter(|(wait, _)| (lo..hi).contains(wait))
                    .map(|(_, spread)| *spread)
                    .collect();
                let mean = in_bucket.iter().map(|&s| f64::from(s)).sum::<f64>()
                    / in_bucket.len().max(1) as f64;
                println!(
                    "  wait {:>5}..{:<5} ms: matches={:>4} mean spread={:>6.1} max spread={}",
                    lo,
                    if hi == u64::MAX { 0 } else { hi },
                    in_bucket.len(),
                    mean,
                    in_bucket.iter().max().copied().unwrap_or(0)
                );
                means.push((in_bucket.len(), mean));
            }
            means
        };
        let widening = simulate(25, 0xC0FFEE);
        let fixed = simulate(0, 0xC0FFEE);
        let wide_means = report("widening (25/s)", &widening);
        let fixed_means = report("fixed window", &fixed);

        // Every match respects the anchor's window at its wait time.
        for (wait, spread) in &widening.spreads {
            let bound = 50 + 25 * (wait / 1000).min(38);
            assert!(
                *spread as u64 <= bound.min(1000),
                "spread {spread} at wait {wait}"
            );
        }
        assert!(fixed.spreads.iter().all(|(_, s)| *s <= 50));
        // Widening trades quality for wait: longer waits give looser matches...
        let (short, long) = (wide_means[0], wide_means[2]);
        assert!(
            short.0 > 0 && long.0 > 0,
            "buckets populated: {wide_means:?}"
        );
        assert!(
            long.1 > short.1,
            "mean spread grows with wait: {short:?} vs {long:?}"
        );
        // ...but far more players get a match than with a fixed window, and fewer time out.
        assert!(widening.matched_players > fixed.matched_players);
        assert!(widening.expired < fixed.expired);
        assert!(fixed_means[0].1 <= 50.0);
        // The widening population still matches the overwhelming majority.
        assert!(
            widening.matched_players as f64 > 0.95 * 1800.0,
            "{}",
            widening.matched_players
        );
    }
}
