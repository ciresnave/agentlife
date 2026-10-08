// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Task Scheduler side of coming back after a logon (DESIGN.md §2.1, DESIGN-REVISION-2 §6.3).
//!
//! Two per-user tasks, both "run only when the user is logged on" (a `wt.exe` tab needs the desktop):
//!
//! * **logon**: `agentlife restore --from-logon`;
//! * **unlock**: `agentlife pending --prompt`, on the session-unlock state change.
//!
//! DESIGN-REVISION-2 §6.3 says "the same task" has the unlock trigger. One task has one action list,
//! and these are two different commands (unlock must never start a restore), so they are two tasks.
//!
//! This module only *builds* the task XML and the `schtasks` argument lists, and waits for the
//! preconditions a logon restore needs. Registering is `agentlife install-task --register`, run by a
//! person; nothing here touches the scheduler.

use std::time::Duration;

pub const LOGON_TASK: &str = "agentlife-logon";
pub const UNLOCK_TASK: &str = "agentlife-unlock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Logon,
    SessionUnlock,
}

impl Trigger {
    pub fn task_name(self) -> &'static str {
        match self {
            Trigger::Logon => LOGON_TASK,
            Trigger::SessionUnlock => UNLOCK_TASK,
        }
    }

    /// The arguments the task passes to `agentlife`.
    pub fn arguments(self) -> &'static str {
        match self {
            Trigger::Logon => "restore --from-logon",
            Trigger::SessionUnlock => "pending --prompt",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Trigger::Logon => {
                "agentlife: bring back the agents that should be running (asks first)"
            }
            Trigger::SessionUnlock => {
                "agentlife: ask about restores that are waiting for an answer"
            }
        }
    }
}

/// Escapes text for an XML element body.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Task Scheduler 1.2 XML for one trigger. `user` is `DOMAIN\name`; `exe` the `agentlife` binary.
///
/// `InteractiveToken` + `LeastPrivilege` is "run only when the user is logged on", without storing a
/// password and without elevation. No execution time limit: a restore waits for a person.
pub fn task_xml(trigger: Trigger, user: &str, exe: &str) -> String {
    let user = xml_escape(user);
    let trigger_xml = match trigger {
        Trigger::Logon => format!(
            "    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>{user}</UserId>\n    </LogonTrigger>\n"
        ),
        Trigger::SessionUnlock => format!(
            "    <SessionStateChangeTrigger>\n      <Enabled>true</Enabled>\n      <StateChange>SessionUnlock</StateChange>\n      <UserId>{user}</UserId>\n    </SessionStateChangeTrigger>\n"
        ),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
  <RegistrationInfo>\n    <Description>{desc}</Description>\n  </RegistrationInfo>\n\
  <Triggers>\n{trigger_xml}  </Triggers>\n\
  <Principals>\n    <Principal id=\"Author\">\n      <UserId>{user}</UserId>\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n\
  <Settings>\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
    <Enabled>true</Enabled>\n\
  </Settings>\n\
  <Actions Context=\"Author\">\n    <Exec>\n      <Command>{exe}</Command>\n      <Arguments>{args}</Arguments>\n    </Exec>\n  </Actions>\n\
</Task>\n",
        desc = xml_escape(trigger.description()),
        exe = xml_escape(exe),
        args = trigger.arguments(),
    )
}

/// The `schtasks` arguments that register a task from an XML file. `/F` replaces an existing task of
/// the same name, so running `install-task --register` twice is harmless.
pub fn create_args(trigger: Trigger, xml_path: &str) -> Vec<String> {
    [
        "/Create",
        "/TN",
        trigger.task_name(),
        "/XML",
        xml_path,
        "/F",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The `schtasks` arguments that remove a task.
pub fn delete_args(trigger: Trigger) -> Vec<String> {
    ["/Delete", "/TN", trigger.task_name(), "/F"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// `schtasks` reads the XML file as UTF-16 with a byte-order mark.
pub fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// What a logon restore waits for, because each is otherwise a silent failure at logon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precondition {
    /// `gh api rate_limit` succeeds (the approvals fetch needs it).
    Network,
    /// The claude-peers broker answers.
    Broker,
    /// The `lane-restart` host program is installed.
    HostProgram,
}

impl Precondition {
    pub const ALL: [Precondition; 3] = [
        Precondition::Network,
        Precondition::Broker,
        Precondition::HostProgram,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Precondition::Network => "network (gh api rate_limit)",
            Precondition::Broker => "claude-peers broker",
            Precondition::HostProgram => "lane-restart host program",
        }
    }
}

pub trait Probe {
    fn holds(&self, p: Precondition) -> bool;
    fn sleep(&self, d: Duration);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitReport {
    /// Preconditions still unmet when the budget ran out (empty = ready).
    pub missing: Vec<Precondition>,
    /// Time spent waiting, as slept (not as measured), so it is deterministic under a fake.
    pub waited: Duration,
}

impl WaitReport {
    pub fn ready(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Waits until every precondition holds or `budget` has been slept. Always probes at least once, so a
/// zero budget is "check now". Each probe round re-checks only what is still missing.
pub fn wait_ready(probe: &dyn Probe, budget: Duration, interval: Duration) -> WaitReport {
    let mut missing: Vec<Precondition> = Precondition::ALL.to_vec();
    let mut waited = Duration::ZERO;
    loop {
        missing.retain(|p| !probe.holds(*p));
        if missing.is_empty() || waited >= budget {
            return WaitReport { missing, waited };
        }
        let step = interval.min(budget - waited).max(Duration::from_millis(1));
        probe.sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn logon_xml_has_the_logon_trigger_and_the_restore_action() {
        let x = task_xml(Trigger::Logon, "PC\\cires", "C:\\bin\\agentlife.exe");
        assert!(x.contains("<LogonTrigger>"));
        assert!(!x.contains("SessionStateChangeTrigger"));
        assert!(x.contains("<Arguments>restore --from-logon</Arguments>"));
        assert!(x.contains("<Command>C:\\bin\\agentlife.exe</Command>"));
        assert!(x.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(x.contains("<UserId>PC\\cires</UserId>"));
    }

    #[test]
    fn unlock_xml_runs_pending_prompt_and_never_restore() {
        let x = task_xml(Trigger::SessionUnlock, "PC\\cires", "agentlife.exe");
        assert!(x.contains("<StateChange>SessionUnlock</StateChange>"));
        assert!(x.contains("<Arguments>pending --prompt</Arguments>"));
        assert!(
            !x.contains("<Arguments>restore"),
            "unlock must never start a restore"
        );
        assert!(!x.contains("<LogonTrigger>"));
    }

    #[test]
    fn user_and_path_are_escaped() {
        let x = task_xml(Trigger::Logon, "A&B\\o<'>", "C:\\a & b\\\"x\".exe");
        assert!(x.contains("A&amp;B\\o&lt;&apos;&gt;"));
        assert!(x.contains("C:\\a &amp; b\\&quot;x&quot;.exe"));
        assert!(!x.contains("A&B"));
    }

    #[test]
    fn schtasks_arguments() {
        assert_eq!(
            create_args(Trigger::Logon, "x.xml"),
            ["/Create", "/TN", "agentlife-logon", "/XML", "x.xml", "/F"]
        );
        assert_eq!(
            delete_args(Trigger::SessionUnlock),
            ["/Delete", "/TN", "agentlife-unlock", "/F"]
        );
    }

    #[test]
    fn utf16_has_a_bom_and_two_bytes_per_unit() {
        assert_eq!(utf16le_with_bom("a€"), [0xFF, 0xFE, b'a', 0, 0xAC, 0x20]);
    }

    struct Fake {
        /// Rounds before each precondition holds; `None` = never.
        ready_after: Vec<(Precondition, Option<u32>)>,
        round: Cell<u32>,
        slept: RefCell<Vec<Duration>>,
        probes: Cell<u32>,
    }

    impl Fake {
        fn new(ready_after: Vec<(Precondition, Option<u32>)>) -> Self {
            Self {
                ready_after,
                round: Cell::new(0),
                slept: RefCell::new(vec![]),
                probes: Cell::new(0),
            }
        }
    }

    impl Probe for Fake {
        fn holds(&self, p: Precondition) -> bool {
            self.probes.set(self.probes.get() + 1);
            self.ready_after
                .iter()
                .any(|(q, r)| *q == p && r.is_some_and(|r| self.round.get() >= r))
        }
        fn sleep(&self, d: Duration) {
            self.slept.borrow_mut().push(d);
            self.round.set(self.round.get() + 1);
        }
    }

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn ready_at_once_does_not_sleep() {
        let f = Fake::new(Precondition::ALL.iter().map(|p| (*p, Some(0))).collect());
        let r = wait_ready(&f, 120 * S, 5 * S);
        assert!(r.ready());
        assert_eq!(r.waited, Duration::ZERO);
        assert!(f.slept.borrow().is_empty());
    }

    #[test]
    fn waits_until_the_last_precondition_holds() {
        let f = Fake::new(vec![
            (Precondition::Network, Some(3)),
            (Precondition::Broker, Some(1)),
            (Precondition::HostProgram, Some(0)),
        ]);
        let r = wait_ready(&f, 120 * S, 5 * S);
        assert!(r.ready());
        assert_eq!(r.waited, 15 * S, "three rounds of 5 s");
    }

    #[test]
    fn a_met_precondition_is_not_probed_again() {
        let f = Fake::new(vec![
            (Precondition::Network, Some(2)),
            (Precondition::Broker, Some(0)),
            (Precondition::HostProgram, Some(0)),
        ]);
        wait_ready(&f, 120 * S, 1 * S);
        // round 0: three probes; rounds 1 and 2: only the network one.
        assert_eq!(f.probes.get(), 3 + 1 + 1);
    }

    #[test]
    fn gives_up_at_the_budget_and_names_what_is_missing() {
        let f = Fake::new(vec![
            (Precondition::Network, None),
            (Precondition::Broker, Some(0)),
            (Precondition::HostProgram, None),
        ]);
        let r = wait_ready(&f, 12 * S, 5 * S);
        assert!(!r.ready());
        assert_eq!(
            r.missing,
            [Precondition::Network, Precondition::HostProgram]
        );
        assert_eq!(r.waited, 12 * S, "5 + 5 + the 2 s that remain");
    }

    #[test]
    fn a_zero_budget_checks_once() {
        let f = Fake::new(vec![(Precondition::Broker, Some(0))]);
        let r = wait_ready(&f, Duration::ZERO, 5 * S);
        assert_eq!(r.missing.len(), 2);
        assert_eq!(r.waited, Duration::ZERO);
        assert!(f.slept.borrow().is_empty());
        assert_eq!(f.probes.get(), 3);
    }
}
