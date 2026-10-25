use engine_net_predict::{
    HookFns, KeyRejection, KeyResolution, KeyValidator, KeyValidatorConfig, KeyVerdict,
    LedgerConfig, PredictError, PredictionHooks, PredictionKey, PredictionLedger, UndoReason,
    decode_verdicts, encode_verdicts,
};

#[derive(Default)]
struct Log(Vec<String>);
impl PredictionHooks for Log {
    fn commit(&mut self, key: PredictionKey) {
        self.0.push(format!("commit {}", key.get()));
    }
    fn undo(&mut self, key: PredictionKey, reason: UndoReason) {
        self.0.push(format!("undo {} {reason:?}", key.get()));
    }
}
impl Log {
    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0)
    }
}

fn ledger() -> PredictionLedger {
    PredictionLedger::new(LedgerConfig {
        max_pending: 8,
        timeout_ticks: 10,
        max_depth: 2,
    })
    .unwrap()
}

#[test]
fn confirm_commits_and_reject_undoes_dependents_newest_first() {
    let mut log = Log::default();
    let mut l = ledger();
    let k1 = l.begin(0, None).unwrap();
    let k2 = l.begin(0, Some(k1)).unwrap();
    let k3 = l.begin(1, Some(k2)).unwrap();
    let k4 = l.begin(1, None).unwrap();
    assert_eq!([k1, k2, k3, k4].map(PredictionKey::get), [1, 2, 3, 4]);
    assert_eq!(l.parent_of(k3), Some(k2));

    assert_eq!(l.confirm(k4, &mut log), KeyResolution::Committed(1));
    assert_eq!(log.take(), ["commit 4"]);
    assert_eq!(l.reject(k1, &mut log), KeyResolution::Undone(3));
    assert_eq!(
        log.take(),
        [
            "undo 3 ParentUndone",
            "undo 2 ParentUndone",
            "undo 1 Rejected"
        ]
    );
    assert_eq!(l.pending_len(), 0);
    assert_eq!(
        l.confirm(k2, &mut log),
        KeyResolution::Unknown,
        "late verdicts are ignored"
    );
    assert!(log.take().is_empty());
}

#[test]
fn child_confirmed_first_commits_only_with_its_parent() {
    let mut log = Log::default();
    let mut l = ledger();
    let parent = l.begin(0, None).unwrap();
    let child = l.begin(0, Some(parent)).unwrap();
    let grandchild = l.begin(0, Some(child)).unwrap();
    assert_eq!(l.confirm(child, &mut log), KeyResolution::Deferred);
    assert!(log.take().is_empty());
    assert_eq!(l.confirm(parent, &mut log), KeyResolution::Committed(2));
    assert_eq!(log.take(), ["commit 1", "commit 2"]);
    assert!(
        l.is_pending(grandchild),
        "unconfirmed descendants stay pending"
    );

    // A deferred child is undone when its parent is rejected.
    let p = l.begin(0, None).unwrap();
    let c = l.begin(0, Some(p)).unwrap();
    assert_eq!(l.confirm(c, &mut log), KeyResolution::Deferred);
    let (mut committed, mut undone) = (Vec::new(), Vec::new());
    let mut hooks = HookFns(
        |k: PredictionKey| committed.push(k.get()),
        |k: PredictionKey, r| undone.push((k.get(), r)),
    );
    assert_eq!(l.reject(p, &mut hooks), KeyResolution::Undone(2));
    assert!(committed.is_empty());
    assert_eq!(
        undone,
        [
            (c.get(), UndoReason::ParentUndone),
            (p.get(), UndoReason::Rejected)
        ]
    );
}

#[test]
fn timeouts_limits_and_clear() {
    let mut log = Log::default();
    let mut l = ledger();
    let a = l.begin(0, None).unwrap();
    let b = l.begin(5, None).unwrap();
    let _a_child = l.begin(6, Some(a)).unwrap();
    assert_eq!(l.expire(9, &mut log), 0);
    assert_eq!(l.expire(10, &mut log), 1);
    assert_eq!(log.take(), ["undo 3 ParentUndone", "undo 1 TimedOut"]);
    assert!(l.is_pending(b));
    assert_eq!(l.stats().timed_out, 1);

    let deep1 = l.begin(6, Some(b)).unwrap();
    let deep2 = l.begin(6, Some(deep1)).unwrap();
    assert!(
        matches!(l.begin(6, Some(deep2)), Err(PredictError::Invalid(_))),
        "max depth"
    );
    assert!(matches!(
        l.begin(6, PredictionKey::new(99)),
        Err(PredictError::Invalid(_))
    ));
    while l.pending_len() < 8 {
        l.begin(7, None).unwrap();
    }
    assert!(matches!(l.begin(7, None), Err(PredictError::Full(_))));
    assert_eq!(l.clear(&mut log), 8);
    assert_eq!(log.take().len(), 8);
    assert_eq!(l.pending_len(), 0);
}

#[test]
fn validator_rejects_replays_and_out_of_window_keys() {
    let mut v = KeyValidator::new(KeyValidatorConfig {
        window: 128,
        max_ahead: 16,
        max_outstanding: 4,
    })
    .unwrap();
    let key = |k| PredictionKey::new(k).unwrap();
    assert_eq!(v.admit(key(17), None), Err(KeyRejection::TooFarAhead));
    v.admit(key(1), None).unwrap();
    assert_eq!(v.admit(key(1), None), Err(KeyRejection::Replayed));
    assert_eq!(
        v.resolve(key(1), true).unwrap(),
        [KeyVerdict {
            key: key(1),
            accepted: true
        }]
    );
    assert_eq!(
        v.admit(key(1), None),
        Err(KeyRejection::Replayed),
        "resolved keys stay used"
    );
    assert_eq!(v.resolve(key(1), true), Err(KeyRejection::NotOutstanding));
    for k in 2..=200 {
        v.admit(key(k), None).unwrap();
        v.resolve(key(k), true).unwrap();
    }
    assert_eq!(v.admit(key(50), None), Err(KeyRejection::TooOld));
    assert_eq!(v.admit(key(200 + 17), None), Err(KeyRejection::TooFarAhead));
    assert_eq!(v.admit(key(150), None), Err(KeyRejection::Replayed));
    // Out-of-order but unseen keys inside the window are fine.
    v.admit(key(210), None).unwrap();
    v.admit(key(205), None).unwrap();

    // Dependent keys: unknown parent, rejected parent, cascade.
    assert_eq!(
        v.admit(key(211), Some(key(209))),
        Err(KeyRejection::UnknownParent)
    );
    assert_eq!(
        v.admit(key(212), Some(key(213))),
        Err(KeyRejection::UnknownParent)
    );
    v.admit(key(214), Some(key(210))).unwrap();
    v.admit(key(215), Some(key(214))).unwrap();
    assert_eq!(
        v.admit(key(216), None),
        Err(KeyRejection::TooManyOutstanding)
    );
    let verdicts = v.resolve(key(210), false).unwrap();
    assert_eq!(
        verdicts
            .iter()
            .map(|v| (v.key.get(), v.accepted))
            .collect::<Vec<_>>(),
        [(210, false), (214, false), (215, false)]
    );
    assert_eq!(v.outstanding_len(), 1);
    assert_eq!(
        v.admit(key(217), Some(key(210))),
        Err(KeyRejection::ParentRejected)
    );
    assert!(KeyRejection::TooOld.notify_client());
    assert!(!KeyRejection::Replayed.notify_client());
}

#[test]
fn end_to_end_verdicts_over_the_wire() {
    let mut log = Log::default();
    let mut client = ledger();
    let mut server = KeyValidator::new(KeyValidatorConfig::default()).unwrap();
    let dash = client.begin(0, None).unwrap();
    let hit = client.begin(0, Some(dash)).unwrap();
    let cheat = client.begin(0, None).unwrap();
    server.admit(dash, None).unwrap();
    server.admit(hit, Some(dash)).unwrap();
    server.admit(cheat, None).unwrap();
    let mut verdicts = server.resolve(hit, true).unwrap();
    verdicts.extend(server.resolve(dash, true).unwrap());
    verdicts.extend(server.resolve(cheat, false).unwrap());
    let bytes = encode_verdicts(&verdicts).unwrap();
    let decoded = decode_verdicts(&bytes).unwrap();
    assert_eq!(decoded, verdicts);
    let outcomes: Vec<_> = decoded.iter().map(|v| client.apply(*v, &mut log)).collect();
    assert_eq!(
        outcomes,
        [
            KeyResolution::Deferred,
            KeyResolution::Committed(2),
            KeyResolution::Undone(1)
        ]
    );
    assert_eq!(log.take(), ["commit 1", "commit 2", "undo 3 Rejected"]);
    assert!(decode_verdicts(&bytes[..bytes.len() - 1]).is_err());
    let mut zero_key = bytes.clone();
    zero_key[3..7].copy_from_slice(&0u32.to_le_bytes());
    assert!(decode_verdicts(&zero_key).is_err());
}
