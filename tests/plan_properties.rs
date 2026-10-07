// SPDX-License-Identifier: MIT OR Apache-2.0
//! Planner properties over many generated registries, and fleet-size runs (40 and 1,000 agents).
//!
//! No external generator: a small deterministic xorshift, so a failing seed is reproducible from
//! the message. Each property is stated once and checked on every seed; the planner unit tests
//! in `src/plan.rs` pin the individual rules, these pin the invariants **between** them.

use agentlife::config::Config;
use agentlife::identity::{ProcessIdentity, ProcessTable};
use agentlife::plan::{build, Inputs, Plan};
use agentlife::registry::{AgentId, AgentRecord, ClosedHow, Intent, Session};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::{BTreeSet, HashMap};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn t(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
}

struct Table(HashMap<u32, u64>);
impl ProcessTable for Table {
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
        self.0.get(&pid).map(|s| ProcessIdentity {
            pid,
            start_secs: *s,
            exe: None,
        })
    }
}

struct World {
    records: Vec<AgentRecord>,
    table: Table,
    missing_cwds: BTreeSet<String>,
}

fn record(i: usize, rng: &mut Rng, pm: bool) -> (AgentRecord, bool) {
    let name = if pm {
        "pm".to_string()
    } else {
        format!("lane{i}")
    };
    let id = if pm {
        "a-pm".to_string()
    } else {
        format!("a-{i}")
    };
    let mut r = AgentRecord::new(
        AgentId::new(&id).unwrap(),
        if pm {
            "C:/Projects".to_string()
        } else {
            format!("C:/Projects/{name}")
        },
        t(0),
    );
    r.name = Some(name.clone());
    let mut args = vec!["claude".to_string(), "--name".to_string(), name.clone()];
    if rng.chance(70) {
        args.push("--dangerously-load-development-channels".into());
        args.push("server:claude-peers".into());
    }
    if rng.chance(15) {
        args.push("--dangerously-skip-permissions".into());
    }
    if rng.chance(10) {
        args.push("--model".into());
        args.push("a;b".into());
    }
    if rng.chance(10) {
        args.push("--add-dir".into());
        args.push("C:/elsewhere".into());
    }
    r.launch_args = if rng.chance(95) { Some(args) } else { None };
    r.permission_mode = match rng.below(6) {
        0 => Some("auto".into()),
        1 => Some("default".into()),
        2 => Some("bypassPermissions".into()),
        3 => Some("plan".into()),
        _ => None,
    };
    r.intent = match rng.below(10) {
        0 => Intent::Lazy,
        1 => Intent::Closed {
            how: ClosedHow::Parked,
            by: "person".into(),
            at: t(0),
        },
        2 => Intent::Closed {
            how: ClosedHow::Exited,
            by: "person".into(),
            at: t(0),
        },
        _ => Intent::Wanted,
    };
    let alive = rng.chance(20);
    r.sessions.push(Session {
        session_id: format!("s{i}"),
        pid: 10_000 + i as u32,
        process_start_secs: if rng.chance(95) {
            Some(5_000 + i as u64)
        } else {
            None
        },
        started_at: t(rng.below(100_000) as i64),
        ended_at: if alive { None } else { Some(t(100_001)) },
        end_reason: None,
    });
    (r, alive)
}

fn world(seed: u64, n: usize) -> World {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut records = Vec::new();
    let mut table = HashMap::new();
    let mut missing = BTreeSet::new();
    let with_pm = rng.chance(80);
    for i in 0..n {
        let (r, alive) = record(i, &mut rng, with_pm && i == 0);
        if alive {
            let s = r.sessions[0].clone();
            // Alive in the table with the recorded start (or any start when none is recorded).
            table.insert(s.pid, s.process_start_secs.unwrap_or(1));
        }
        if rng.chance(8) {
            missing.insert(r.launch_cwd.clone());
        }
        records.push(r);
    }
    World {
        records,
        table: Table(table),
        missing_cwds: missing,
    }
}

fn config(rng: &mut Rng) -> Config {
    Config {
        batch_size: 1 + rng.below(6) as u32,
        tabs_per_window: 1 + rng.below(12) as u32,
        max_running: if rng.chance(50) {
            Some(rng.below(40) as u32)
        } else {
            None
        },
        ..Config::default()
    }
}

fn plan(w: &World, c: &Config, free: Option<f64>) -> Plan {
    build(&Inputs {
        records: &w.records,
        table: &w.table,
        cfg: c,
        free_ram_gb: free,
        cwd_exists: &|p| !w.missing_cwds.contains(p),
        priority: &[],
        only: None,
    })
}

#[test]
fn the_planner_invariants_hold_on_many_generated_registries() {
    for seed in 1..=400u64 {
        let mut rng = Rng(seed * 7919);
        let n = 1 + rng.below(60) as usize;
        let w = world(seed, n);
        let c = config(&mut rng);
        let free = [None, Some(100.0), Some(1.0)][rng.below(3) as usize];
        let p = plan(&w, &c, free);
        let ctx = format!(
            "seed {seed}, n {n}, batch {}, tabs {}, max {:?}, free {free:?}",
            c.batch_size, c.tabs_per_window, c.max_running
        );

        // 1. Every record is accounted for exactly once.
        let mut seen: Vec<&str> = p
            .entries
            .iter()
            .map(|e| e.agent_id.as_str())
            .chain(p.held.iter().map(|h| h.agent_id.as_str()))
            .chain(p.excluded.iter().map(|x| x.agent_id.as_str()))
            .collect();
        seen.sort();
        let mut want: Vec<&str> = w.records.iter().map(|r| r.agent_id.as_str()).collect();
        want.sort();
        assert_eq!(seen, want, "a record was lost or counted twice: {ctx}");

        let by_id: HashMap<&str, &AgentRecord> =
            w.records.iter().map(|r| (r.agent_id.as_str(), r)).collect();

        // 2. Only wanted, not-alive agents are ever in the plan or held.
        for id in p
            .entries
            .iter()
            .map(|e| &e.agent_id)
            .chain(p.held.iter().map(|h| &h.agent_id))
        {
            let r = by_id[id.as_str()];
            assert!(
                matches!(r.intent, Intent::Wanted),
                "{id} is not wanted: {ctx}"
            );
            let s = &r.sessions[0];
            assert!(
                s.ended_at.is_some() || w.table.identity_of(s.pid).is_none(),
                "{id} is alive but planned: {ctx}"
            );
        }

        // 3. The PM is first and alone; every other batch is within batch_size.
        let pm_first = p.entries.first().is_some_and(|e| e.pm);
        if pm_first {
            assert_eq!(
                p.entries.iter().filter(|e| e.batch == 0).count(),
                1,
                "{ctx}"
            );
        }
        assert!(
            p.entries.iter().skip(1).all(|e| !e.pm),
            "a PM that is not first: {ctx}"
        );
        let mut sizes: HashMap<u32, u32> = HashMap::new();
        for e in p.entries.iter().filter(|e| !(pm_first && e.batch == 0)) {
            *sizes.entry(e.batch).or_default() += 1;
        }
        assert!(
            sizes.values().all(|n| *n <= c.batch_size),
            "a batch is too big: {ctx}"
        );
        let batches: Vec<u32> = p.entries.iter().map(|e| e.batch).collect();
        assert!(
            batches.windows(2).all(|w| w[1] == w[0] || w[1] == w[0] + 1),
            "{ctx}"
        );

        // 4. Hard rules hold for everything that would be launched.
        for e in &p.entries {
            assert!(
                e.argv.iter().all(|a| !a.contains(';')),
                "a semicolon got through: {ctx}"
            );
            if e.mode.as_deref() == Some("bypassPermissions") {
                assert!(e.pm, "bypass for a non-PM: {ctx}");
            }
            assert!(e.cwd.starts_with("C:/Projects"), "{ctx}");
            assert!(
                !w.missing_cwds.contains(&e.cwd),
                "a missing cwd planned: {ctx}"
            );
        }

        // 5. Caps.
        if let Some(m) = c.max_running {
            assert!(
                p.entries.len() as u32 + p.running_now <= m.max(p.running_now),
                "max_running exceeded: {ctx}"
            );
        }
        if free.is_some_and(|g| g < c.free_ram_floor_gb) {
            assert!(
                p.entries.is_empty(),
                "started with memory below the floor: {ctx}"
            );
        }

        // 6. Placement.
        for (idx, e) in p.entries.iter().enumerate() {
            assert!(e.tab < c.tabs_per_window, "{ctx}");
            assert_eq!(e.window, idx as u32 / c.tabs_per_window, "{ctx}");
        }

        // 7. The hash is valid, and independent of the order the registry listed things in.
        assert!(p.hash_is_valid(), "{ctx}");
        let mut shuffled = World {
            records: w.records.clone(),
            table: Table(w.table.0.clone()),
            missing_cwds: w.missing_cwds.clone(),
        };
        let mut r2 = Rng(seed ^ 0xABCD);
        for i in (1..shuffled.records.len()).rev() {
            let j = r2.below(i as u64 + 1) as usize;
            shuffled.records.swap(i, j);
        }
        assert_eq!(
            plan(&shuffled, &c, free).hash,
            p.hash,
            "order-dependent plan: {ctx}"
        );
    }
}

#[test]
fn starting_what_a_plan_names_makes_the_next_plan_empty_of_it() {
    // Idempotence: mark every planned agent alive, plan again; none of them may reappear.
    for seed in 1..=150u64 {
        let mut rng = Rng(seed * 104_729);
        let n = 1 + rng.below(50) as usize;
        let mut w = world(seed, n);
        let mut c = config(&mut rng);
        c.max_running = None;
        let first = plan(&w, &c, None);
        let started: BTreeSet<String> = first.entries.iter().map(|e| e.agent_id.clone()).collect();
        for r in w
            .records
            .iter_mut()
            .filter(|r| started.contains(r.agent_id.as_str()))
        {
            let s = r.sessions.last_mut().unwrap();
            s.ended_at = None;
            s.process_start_secs = Some(777);
            w.table.0.insert(s.pid, 777);
        }
        let second = plan(&w, &c, None);
        for e in &second.entries {
            assert!(
                !started.contains(&e.agent_id),
                "seed {seed}: {} would start twice",
                e.agent_id
            );
        }
        assert_eq!(
            second.running_now,
            first.running_now + started.len() as u32,
            "seed {seed}"
        );
    }
}

fn fleet(n: usize) -> World {
    let mut records = Vec::new();
    for i in 0..n {
        let name = format!("lane{i:04}");
        let mut r = AgentRecord::new(
            AgentId::new(format!("a-{i:04}")).unwrap(),
            format!("C:/Projects/{name}"),
            t(0),
        );
        r.name = Some(name.clone());
        r.launch_args = Some(
            [
                "claude",
                "--name",
                &name,
                "--dangerously-load-development-channels",
                "server:claude-peers",
            ]
            .map(String::from)
            .to_vec(),
        );
        r.permission_mode = Some("auto".into());
        r.sessions.push(Session {
            session_id: format!("s{i}"),
            pid: 20_000 + i as u32,
            process_start_secs: Some(1),
            started_at: t(i as i64),
            ended_at: Some(t(i as i64 + 1)),
            end_reason: None,
        });
        records.push(r);
    }
    World {
        records,
        table: Table(HashMap::new()),
        missing_cwds: BTreeSet::new(),
    }
}

fn fleet_run(n: usize, batch: u32, tabs: u32) {
    let w = fleet(n);
    let c = Config {
        batch_size: batch,
        tabs_per_window: tabs,
        ..Config::default()
    };
    let began = std::time::Instant::now();
    let p = plan(&w, &c, Some(64.0));
    let took = began.elapsed();
    assert_eq!(p.entries.len(), n);
    assert_eq!(p.batch_count() as usize, n.div_ceil(batch as usize));
    assert_eq!(
        p.entries.iter().map(|e| e.window).max().unwrap() as usize + 1,
        n.div_ceil(tabs as usize),
        "windows are created on demand, one per `tabs_per_window`"
    );
    // Most recently active first: the highest index was active last, so it is first.
    assert_eq!(p.entries[0].agent_id, format!("a-{:04}", n - 1));
    assert_eq!(
        p.estimate_secs(),
        u64::from(p.batch_count()) * (c.batch_delay_secs + 12)
    );
    assert!(p.hash_is_valid());
    assert!(
        took < std::time::Duration::from_secs(5),
        "{n} agents took {took:?} to plan"
    );
}

#[test]
fn a_fleet_of_40_plans_into_the_expected_batches_and_windows() {
    fleet_run(40, 3, 10);
}

#[test]
fn a_fleet_of_1000_plans_in_bounded_time_and_a_cap_holds_the_rest() {
    fleet_run(1000, 5, 10);
    let w = fleet(1000);
    let c = Config {
        max_running: Some(300),
        ..Config::default()
    };
    let p = plan(&w, &c, None);
    assert_eq!((p.entries.len(), p.held.len()), (300, 700));
    assert!(p.held.iter().all(|h| h.why == "max_running"));
}
