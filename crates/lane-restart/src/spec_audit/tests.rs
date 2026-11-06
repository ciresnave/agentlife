use super::*;
use crate::spec_synapse::{DEV_CHANNELS, MCP_CONFIG, PEERS, SYNAPSE};
use tempfile::tempdir;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn write_spec(dir: &Path, role: &str, args: &[&str]) {
    let s = spec::SpecFile {
        schema: spec::SCHEMA,
        role: lane_state::role::Role::new(role).unwrap(),
        name: lane_state::role::Role::new(role).unwrap(),
        cwd: format!("C:/Projects/{role}"),
        model: "sonnet".into(),
        permission_mode: None,
        remote_control: false,
        launch_args: v(args),
        handoff_path: None,
        handoff_mtime: None,
        state: spec::SpecState::Running,
        written_at: Utc::now(),
        written_by: "test".into(),
        stopped_session_id: None,
        stopped_pid_start_secs: None,
        stop_reason: None,
        stop_kind: None,
        stop_mac: None,
        tool_version: "test".into(),
        launch: None,
        wsl: None,
    };
    spec::write_atomic(dir, &s).unwrap();
}

fn b64enc(b: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, x)| a | (u32::from(*x) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn lp(b: &mut Vec<u8>, x: &[u8]) {
    b.extend((x.len() as u32).to_be_bytes());
    b.extend(x);
}

/// A certificate in the known shape, expiring at `exp`.
fn cert_pem(role: &str, exp: DateTime<Utc>) -> String {
    let mut b = Vec::new();
    lp(&mut b, b"synapse/agent-cert/v1");
    b.extend([0, 0, 0, 1, 1]);
    lp(&mut b, &[7; 16]);
    lp(&mut b, b"abcd");
    lp(&mut b, role.as_bytes());
    lp(&mut b, format!("{role}@acct").as_bytes());
    lp(&mut b, &[1; 32]);
    lp(&mut b, &[2; 32]);
    lp(&mut b, &(exp.timestamp_micros() - 1_000_000).to_be_bytes());
    lp(&mut b, &exp.timestamp_micros().to_be_bytes());
    format!(
        "-----BEGIN SYNAPSE AGENT CERT-----\n{}\n-----END SYNAPSE AGENT CERT-----\n",
        b64enc(&b)
    )
}

/// A lane with a spec, an mcp.json naming `home`, and (when `exp` is given) a certificate.
fn lane(dir: &Path, home: &Path, role: &str, args: &[&str], exp: Option<DateTime<Utc>>) {
    write_spec(dir, role, args);
    std::fs::write(
        dir.join(format!("{role}.mcp.json")),
        format!(
            r#"{{"mcpServers":{{"synapse":{{"command":"x","args":["--role","{role}","--home","{}"]}}}}}}"#,
            home.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    if let Some(e) = exp {
        let d = home.join("roles").join(role);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("cert.pem"), cert_pem(role, e)).unwrap();
    }
}

const BOTH: &[&str] = &[MCP_CONFIG, "m.json", DEV_CHANNELS, PEERS, SYNAPSE];
const PEERS_ONLY: &[&str] = &[MCP_CONFIG, "m.json", DEV_CHANNELS, PEERS];
const SYN_ONLY: &[&str] = &[MCP_CONFIG, "m.json", DEV_CHANNELS, SYNAPSE];

fn none() -> PresenceMap {
    Err("not asked".into())
}

fn sev(rows: &[AuditRow], role: &str, now: DateTime<Utc>) -> Vec<(Severity, String)> {
    findings(rows.iter().find(|r| r.role == role).unwrap(), now)
}

#[test]
fn the_certificate_expiry_is_read_back_exactly_and_a_wrong_role_or_shape_is_unreadable() {
    let exp = DateTime::from_timestamp_micros(1_790_000_000_123_456).unwrap();
    let pem = cert_pem("kiss", exp);
    assert_eq!(cert_expiry(&pem, "kiss"), Some(exp));
    assert_eq!(
        cert_expiry(&pem, "other"),
        None,
        "a certificate for another role"
    );
    assert_eq!(
        cert_expiry("-----BEGIN-----\nAAAA\n-----END-----", "kiss"),
        None
    );
    assert_eq!(cert_expiry("", "kiss"), None);
}

#[test]
fn the_expiry_field_of_the_real_2026_11_04_certificate_reads_as_one_day_after_issue() {
    // The two timestamps read by hand from the agentlife certificate of 2026-11-04 (a certificate
    // is public by design); the rest of the body here is filler of the same shape.
    let mut b = Vec::new();
    lp(&mut b, b"synapse/agent-cert/v1");
    b.extend([0, 0, 0, 1, 1]);
    let fields: [&[u8]; 6] = [
        &[9; 16],
        b"6330",
        b"agentlife",
        b"agentlife@ciresnave",
        &[1; 32],
        &[2; 32],
    ];
    for x in fields {
        lp(&mut b, x);
    }
    for t in [0x00065f7404cf6e9f_u64, 0x00065f8822a6ce9f_u64] {
        lp(&mut b, &t.to_be_bytes());
    }
    let pem = format!("-----BEGIN X-----\n{}\n-----END X-----", b64enc(&b));
    let exp = cert_expiry(&pem, "agentlife").unwrap();
    assert_eq!(exp.timestamp_micros(), 0x00065f8822a6ce9f_i64);
    assert_eq!(exp.format("%Y-%m").to_string(), "2026-11");
    assert_eq!(
        (0x00065f8822a6ce9f_i64 - 0x00065f7404cf6e9f_i64) / 1_000_000 / 3600 / 24,
        1
    );
}

#[test]
fn each_launch_shape_is_classified_and_only_peers_only_or_no_channel_fails() {
    let (d, h) = (tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    let ok = Some(now + chrono::Duration::days(10));
    lane(d.path(), h.path(), "both", BOTH, ok);
    lane(d.path(), h.path(), "syn", SYN_ONLY, ok);
    lane(d.path(), h.path(), "peers", PEERS_ONLY, ok);
    lane(d.path(), h.path(), "bare", &[], ok);
    let rows = audit(d.path(), &[], &none(), now);
    assert_eq!(
        rows.iter().map(|r| r.role.as_str()).collect::<Vec<_>>(),
        ["bare", "both", "peers", "syn"]
    );
    assert!(sev(&rows, "both", now).is_empty());
    assert!(sev(&rows, "syn", now).is_empty());
    let p = sev(&rows, "peers", now);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].0, Severity::Fail);
    assert!(p[0].1.starts_with("peers-only"), "{p:?}");
    assert!(sev(&rows, "bare", now)[0].1.contains("no channel"));
    let (text, ok) = render(&rows, &none(), now);
    assert!(!ok);
    assert!(
        text.contains("AUDIT 4 role(s): 2 failing, 0 warning, 2 clean"),
        "{text}"
    );
    assert!(text.contains("mcp+peers+synapse"), "{text}");
    assert!(text.contains("presence unavailable: not asked"), "{text}");
}

#[test]
fn a_missing_mcp_json_identity_home_or_certificate_fails_and_names_which() {
    let (d, h) = (tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    write_spec(d.path(), "nomcp", BOTH);
    lane(d.path(), h.path(), "nohome", BOTH, None);
    lane(d.path(), h.path(), "nocert", BOTH, None);
    std::fs::create_dir_all(h.path().join("roles").join("nocert")).unwrap();
    lane(
        d.path(),
        h.path(),
        "old",
        BOTH,
        Some(now - chrono::Duration::hours(1)),
    );
    lane(
        d.path(),
        h.path(),
        "soon",
        BOTH,
        Some(now + chrono::Duration::hours(5)),
    );
    let rows = audit(d.path(), &[], &none(), now);
    assert!(sev(&rows, "nomcp", now)[0].1.contains("no mcp.json"));
    assert!(sev(&rows, "nohome", now)
        .iter()
        .any(|f| f.1 == "identity home missing"));
    assert!(sev(&rows, "nocert", now)
        .iter()
        .any(|f| f.1 == "no certificate"));
    let old = sev(&rows, "old", now);
    assert_eq!(old[0].0, Severity::Fail);
    assert!(old[0].1.starts_with("certificate expired"));
    let soon = sev(&rows, "soon", now);
    assert_eq!(soon.len(), 1);
    assert_eq!(
        soon[0].0,
        Severity::Warn,
        "near expiry warns and does not fail"
    );
}

#[test]
fn a_malformed_mcp_json_or_unreadable_spec_is_a_finding_not_a_crash() {
    let (d, h) = (tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    lane(
        d.path(),
        h.path(),
        "bad",
        BOTH,
        Some(now + chrono::Duration::days(9)),
    );
    std::fs::write(d.path().join("bad.mcp.json"), "{nope").unwrap();
    std::fs::write(d.path().join("junk.launch.json"), "{nope").unwrap();
    let rows = audit(d.path(), &[], &none(), now);
    assert!(sev(&rows, "bad", now)[0].1.contains("not valid JSON"));
    assert!(sev(&rows, "junk", now)[0]
        .1
        .contains("no usable launch spec"));
}

#[test]
fn presence_is_parsed_from_the_list_lines_and_stuck_mail_to_an_offline_role_warns() {
    let text = concat!(
        r#"{"global_id":"a@acct","online":false,"pending":{"dead":0,"leased":0,"oldest_enqueued_at":"2026-11-04T10:00:00+00:00","queued":3}}"#,
        "\n",
        r#"{"global_id":"b@acct","online":true,"pending":{"queued":0,"oldest_enqueued_at":null}}"#,
        "\n"
    );
    let p = parse_presence(text).unwrap();
    assert_eq!(p["a"].queued, 3);
    assert!(!p["a"].online && p["b"].online);
    assert!(parse_presence("not json").is_err());
    let (d, h) = (tempdir().unwrap(), tempdir().unwrap());
    let now = DateTime::parse_from_rfc3339("2026-11-04T20:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    let e = Some(now + chrono::Duration::days(9));
    lane(d.path(), h.path(), "a", BOTH, e);
    lane(d.path(), h.path(), "b", BOTH, e);
    let rows = audit(d.path(), &[], &Ok(p.clone()), now);
    let a = sev(&rows, "a", now);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].0, Severity::Warn);
    assert!(a[0].1.contains("3 message(s) queued, oldest 10 h"), "{a:?}");
    assert!(sev(&rows, "b", now).is_empty());
    assert_eq!(rows[0].presence, p.get("a").cloned());
}

#[test]
fn naming_roles_audits_only_those_and_the_common_home_is_the_most_named() {
    let (d, h) = (tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    let e = Some(now + chrono::Duration::days(9));
    lane(d.path(), h.path(), "x", BOTH, e);
    lane(d.path(), h.path(), "y", BOTH, e);
    let rows = audit(d.path(), &v(&["y"]), &none(), now);
    assert_eq!(rows.len(), 1);
    let all = audit(d.path(), &[], &none(), now);
    assert_eq!(
        common_home(&all).unwrap(),
        PathBuf::from(h.path().to_string_lossy().replace('\\', "/"))
    );
    assert_eq!(common_home(&[]), None);
}

fn account_key(home: &Path) {
    let d = home.join("account");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("account.key.pem"), "k").unwrap();
}

fn cert_findings(rows: &[AuditRow], role: &str, now: DateTime<Utc>) -> Vec<(Severity, String)> {
    sev(rows, role, now)
        .into_iter()
        .filter(|f| f.1.starts_with("certificate"))
        .collect()
}

#[test]
fn an_account_home_role_with_an_aged_certificate_is_info_and_a_role_only_home_still_fails() {
    let (d, full, only) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    account_key(full.path());
    let old = Some(now - chrono::Duration::hours(7));
    let soon = Some(now + chrono::Duration::hours(2));
    lane(d.path(), full.path(), "fullold", BOTH, old);
    lane(d.path(), full.path(), "fullsoon", BOTH, soon);
    lane(d.path(), only.path(), "onlyold", BOTH, old);
    lane(d.path(), only.path(), "onlysoon", BOTH, soon);
    // Online and offline make no difference to a full home: the key renews the certificate.
    let online =
        parse_presence(r#"{"global_id":"fullold@a","online":true,"pending":{"queued":0}}"#);
    for pres in [online, none()] {
        let rows = audit(d.path(), &[], &pres, now);
        let f = cert_findings(&rows, "fullold", now);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, Severity::Info, "{f:?}");
        assert!(f[0].1.contains("renews on next start"));
        let s = cert_findings(&rows, "fullsoon", now);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, Severity::Info);
        assert_eq!(cert_findings(&rows, "onlyold", now)[0].0, Severity::Fail);
        assert_eq!(cert_findings(&rows, "onlysoon", now)[0].0, Severity::Warn);
    }
}

#[test]
fn an_info_finding_makes_neither_a_failing_nor_a_warning_row_and_a_missing_cert_still_fails() {
    let (d, full) = (tempdir().unwrap(), tempdir().unwrap());
    let now = Utc::now();
    account_key(full.path());
    lane(
        d.path(),
        full.path(),
        "fullold",
        BOTH,
        Some(now - chrono::Duration::hours(7)),
    );
    lane(d.path(), full.path(), "nocert", BOTH, None);
    std::fs::create_dir_all(full.path().join("roles").join("nocert")).unwrap();
    let rows = audit(d.path(), &["fullold".to_string()], &none(), now);
    let (text, ok) = render(&rows, &none(), now);
    assert!(ok, "{text}");
    assert!(
        text.contains("AUDIT 1 role(s): 0 failing, 0 warning, 1 clean"),
        "{text}"
    );
    let rows = audit(d.path(), &["nocert".to_string()], &none(), now);
    assert!(sev(&rows, "nocert", now)
        .iter()
        .any(|f| f.0 == Severity::Fail && f.1 == "no certificate"));
}
