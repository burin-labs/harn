//! Deterministic public-restore schedules at the existing writer boundaries.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use super::{CodeIndexCapability, CodeIndexSnapshot, SharedIndex};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Actor {
    Background(usize),
    Restore,
    Waiter(usize),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Point {
    BeforeCapture,
    AfterCapture,
    Replaced,
    Waiting(usize),
}

type Event = (Actor, Point);
static SERIAL: Mutex<()> = Mutex::new(());
static ACTIVE: Mutex<Option<Arc<Probe>>> = Mutex::new(None);
thread_local! { static BACKGROUND: Cell<Option<usize>> = const { Cell::new(None) }; }

struct Probe {
    root: PathBuf,
    warm: usize,
    next_background: AtomicUsize,
    events: mpsc::Sender<Event>,
    pauses: Mutex<HashMap<Event, mpsc::Receiver<()>>>,
}

struct Schedule {
    probe: Arc<Probe>,
    events: mpsc::Receiver<Event>,
    releases: HashMap<Event, mpsc::Sender<()>>,
    pending: VecDeque<Event>,
}

impl Schedule {
    fn new(root: &Path, cap: &CodeIndexCapability, pauses: &[Event]) -> Self {
        let (tx, events) = mpsc::channel();
        let mut receivers = HashMap::new();
        let releases = pauses
            .iter()
            .map(|event| {
                let (tx, rx) = mpsc::channel();
                receivers.insert(*event, rx);
                (*event, tx)
            })
            .collect();
        let probe = Arc::new(Probe {
            root: root.canonicalize().unwrap(),
            warm: Arc::as_ptr(&cap.warm) as usize,
            next_background: AtomicUsize::new(0),
            events: tx,
            pauses: Mutex::new(receivers),
        });
        assert!(ACTIVE.lock().unwrap().replace(probe.clone()).is_none());
        Self {
            probe,
            events,
            releases,
            pending: VecDeque::new(),
        }
    }

    fn next(&mut self, predicate: impl Fn(Event) -> bool) -> Event {
        if let Some(index) = self.pending.iter().position(|event| predicate(*event)) {
            return self.pending.remove(index).unwrap();
        }
        loop {
            let event = self.events.recv().expect("owning boundary event");
            if predicate(event) {
                return event;
            }
            self.pending.push_back(event);
        }
    }

    fn reached(&mut self, expected: Event) {
        self.next(|event| event == expected);
    }

    fn release(&mut self, event: Event) {
        self.releases
            .remove(&event)
            .expect("one paused boundary")
            .send(())
            .unwrap();
    }
}

impl Drop for Schedule {
    fn drop(&mut self) {
        for (_, release) in self.releases.drain() {
            let _ = release.send(());
        }
        let mut active = ACTIVE.lock().unwrap();
        assert!(active
            .as_ref()
            .is_some_and(|probe| Arc::ptr_eq(probe, &self.probe)));
        *active = None;
    }
}

fn actor() -> Option<Actor> {
    match thread::current().name() {
        Some("9771-restore-b") => Some(Actor::Restore),
        Some("9771-waiter-1") => Some(Actor::Waiter(1)),
        Some("9771-waiter-2") => Some(Actor::Waiter(2)),
        _ => BACKGROUND.get().map(Actor::Background),
    }
}

fn probe(root: &Path) -> Option<Arc<Probe>> {
    ACTIVE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|probe| probe.root == root)
        .cloned()
}

fn fire(probe: &Probe, actor: Actor, point: Point) {
    let event = (actor, point);
    let pause = probe.pauses.lock().unwrap().remove(&event);
    eprintln!("snapshot schedule boundary: {actor:?} {point:?}");
    let _ = probe.events.send(event);
    if let Some(pause) = pause {
        let _ = pause.recv();
    }
}

pub(super) fn before_capture(index: &SharedIndex) {
    let root = index
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.root.clone());
    if let Some(probe) = root.as_deref().and_then(probe) {
        let id = probe.next_background.fetch_add(1, Ordering::SeqCst);
        BACKGROUND.set(Some(id));
        fire(&probe, Actor::Background(id), Point::BeforeCapture);
    }
}

pub(super) fn after_capture(root: &Path) {
    if let (Some(probe), Some(actor)) = (probe(root), actor()) {
        fire(&probe, actor, Point::AfterCapture);
    }
}

pub(super) fn after_replace(root: &Path) {
    if let (Some(probe), Some(actor)) = (probe(root), actor()) {
        fire(&probe, actor, Point::Replaced);
    }
}

pub(super) fn waiting(warm: &super::warm::WarmCoordinator, pending: usize) {
    let probe = ACTIVE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|probe| probe.warm == std::ptr::from_ref(warm) as usize)
        .cloned();
    if let (Some(probe), Some(actor)) = (probe, actor()) {
        fire(&probe, actor, Point::Waiting(pending));
    }
}

fn write(root: &Path, index: usize, prefix: &str) {
    std::fs::write(
        root.join(format!("src/f{index:04}.rs")),
        format!("pub fn {prefix}_{index}() -> usize {{ {index} }}\n"),
    )
    .unwrap();
}

fn seed(files: usize) -> (tempfile::TempDir, CodeIndexCapability) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    for index in 0..files {
        write(dir.path(), index, "original");
    }
    let cap = CodeIndexCapability::new();
    cap.warm_session(dir.path());
    cap.wait_until_idle();
    assert_eq!(
        CodeIndexSnapshot::load(dir.path())
            .unwrap()
            .unwrap()
            .files
            .len(),
        files
    );
    (dir, cap)
}

fn newer_nodes(root: &Path) -> usize {
    CodeIndexSnapshot::load(root)
        .unwrap()
        .unwrap()
        .symbols
        .unwrap()
        .nodes
        .iter()
        .filter(|node| node.name.starts_with("newer_marker_"))
        .count()
}

fn ordered_public_restores(point: Point) {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, cap) = seed(520);
    let mut schedule = Schedule::new(
        dir.path(),
        &cap,
        &[
            (Actor::Background(0), point),
            (Actor::Restore, Point::Replaced),
        ],
    );
    write(dir.path(), 0, "first_edit_requires_persistence");
    assert!(cap.restore_from_disk(dir.path()).unwrap());
    schedule.reached((Actor::Background(0), point));
    for index in 0..520 {
        write(dir.path(), index, "newer_marker");
    }
    let second = cap.clone();
    let root = dir.path().to_path_buf();
    let restore = harn_vm::runtime_stack::builder()
        .name("9771-restore-b".into())
        .spawn(move || second.restore_from_disk(&root).unwrap())
        .unwrap();
    let boundary = schedule.next(|(actor, point)| {
        actor == Actor::Restore && matches!(point, Point::Waiting(_) | Point::Replaced)
    });
    let admission_waited = matches!(boundary.1, Point::Waiting(_));
    if admission_waited {
        schedule.release((Actor::Background(0), point));
        schedule.reached((Actor::Restore, Point::Replaced));
    }
    let checkpoint_nodes = newer_nodes(dir.path());
    assert!(checkpoint_nodes > 0, "real newer checkpoint reached");
    if !admission_waited {
        schedule.release((Actor::Background(0), point));
        schedule.reached((Actor::Background(0), Point::Replaced));
    }
    let after_older_writer = newer_nodes(dir.path());
    schedule.release((Actor::Restore, Point::Replaced));
    assert!(restore.join().unwrap());
    cap.wait_until_idle();
    eprintln!("public restore ordering: {point:?} admission_waited={admission_waited} checkpoint_nodes={checkpoint_nodes} after_older_writer={after_older_writer}");
    assert_eq!(
        after_older_writer, checkpoint_nodes,
        "older async capture/write replaced a newer real checkpoint"
    );
}

#[test]
fn public_restore_preserves_checkpoint_before_async_capture() {
    ordered_public_restores(Point::BeforeCapture);
}

#[test]
fn public_restore_preserves_checkpoint_after_async_capture() {
    ordered_public_restores(Point::AfterCapture);
}

#[test]
fn warm_session_does_not_wait_for_another_roots_public_restore() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, _) = seed(520);
    for index in 0..520 {
        write(dir.path(), index, "newer_marker");
    }
    let cap = CodeIndexCapability::new();
    let mut schedule = Schedule::new(dir.path(), &cap, &[(Actor::Restore, Point::Replaced)]);
    let second = cap.clone();
    let root = dir.path().to_path_buf();
    let restore = harn_vm::runtime_stack::builder()
        .name("9771-restore-b".into())
        .spawn(move || second.restore_from_disk(&root).unwrap())
        .unwrap();
    schedule.reached((Actor::Restore, Point::Replaced));
    assert!(
        cap.index.lock().unwrap().is_none(),
        "restore has not published"
    );
    let other = tempfile::tempdir().unwrap();
    assert_eq!(
        cap.warm_session(other.path()),
        super::SessionWarmOutcome::Building
    );
    assert!(
        cap.index.lock().unwrap().is_none(),
        "warm returned before restore publication"
    );
    schedule.release((Actor::Restore, Point::Replaced));
    assert!(restore.join().unwrap());
    cap.wait_until_idle();
    eprintln!("nonblocking warm: returned Building while another root's real restore checkpoint was paused before publication");
}

#[test]
fn explicit_persist_captures_the_root_published_while_it_waited() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (original, cap) = seed(2);
    let original_path = CodeIndexSnapshot::path_for(original.path());
    let original_bytes = std::fs::read(&original_path).unwrap();
    let (next, _) = seed(520);
    for index in 0..520 {
        write(next.path(), index, "newer_marker");
    }
    let mut schedule = Schedule::new(next.path(), &cap, &[(Actor::Restore, Point::Replaced)]);
    let second = cap.clone();
    let root = next.path().to_path_buf();
    let restore = harn_vm::runtime_stack::builder()
        .name("9771-restore-b".into())
        .spawn(move || second.restore_from_disk(&root).unwrap())
        .unwrap();
    schedule.reached((Actor::Restore, Point::Replaced));
    let writer = cap.clone();
    let persist = harn_vm::runtime_stack::builder()
        .name("9771-waiter-1".into())
        .spawn(move || writer.persist_to_disk().unwrap())
        .unwrap();
    schedule.reached((Actor::Waiter(1), Point::Waiting(0)));
    schedule.release((Actor::Restore, Point::Replaced));
    assert!(restore.join().unwrap());
    assert!(persist.join().unwrap());
    cap.wait_until_idle();
    assert_eq!(
        cap.index.lock().unwrap().as_ref().unwrap().root,
        next.path().canonicalize().unwrap()
    );
    assert_eq!(std::fs::read(original_path).unwrap(), original_bytes);
    assert_eq!(newer_nodes(next.path()), 520);
    eprintln!("explicit persist: waited for real restore, captured the newly published root, preserved original snapshot and all520 newer nodes");
}

#[test]
fn failed_restore_releases_admission_for_public_warm() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    write(dir.path(), 0, "newer_marker");
    let snapshot = CodeIndexSnapshot::path_for(dir.path());
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    std::fs::write(snapshot, b"invalid snapshot").unwrap();
    let cap = CodeIndexCapability::new();
    assert!(cap.restore_from_disk(dir.path()).is_err());
    assert_eq!(
        cap.warm_session(dir.path()),
        super::SessionWarmOutcome::Building
    );
    cap.wait_until_idle();
    assert_eq!(newer_nodes(dir.path()), 1);
    eprintln!("restore error cleanup: invalid snapshot refused, public warm rebuilt and persisted actual newer node");
}

#[test]
fn two_public_waiters_wait_for_both_actual_snapshot_writes() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, cap) = seed(2);
    write(dir.path(), 0, "newer_marker");
    assert!(cap.restore_from_disk(dir.path()).unwrap());
    cap.wait_until_idle();
    let mut schedule = Schedule::new(
        dir.path(),
        &cap,
        &[
            (Actor::Background(0), Point::AfterCapture),
            (Actor::Background(1), Point::AfterCapture),
        ],
    );
    cap.warm.persist_in_background(cap.index.clone());
    schedule.reached((Actor::Background(0), Point::AfterCapture));
    cap.warm.persist_in_background(cap.index.clone());
    let (returned, returns) = mpsc::channel();
    let waiters: Vec<_> = (1..=2)
        .map(|number| {
            let cap = cap.clone();
            let returned = returned.clone();
            harn_vm::runtime_stack::builder()
                .name(format!("9771-waiter-{number}"))
                .spawn(move || {
                    cap.wait_until_idle();
                    returned.send(number).unwrap();
                })
                .unwrap()
        })
        .collect();
    for number in 1..=2 {
        schedule.reached((Actor::Waiter(number), Point::Waiting(2)));
    }
    assert!(matches!(returns.try_recv(), Err(mpsc::TryRecvError::Empty)));
    schedule.release((Actor::Background(0), Point::AfterCapture));
    schedule.reached((Actor::Background(1), Point::AfterCapture));
    for number in 1..=2 {
        schedule.reached((Actor::Waiter(number), Point::Waiting(1)));
    }
    assert!(matches!(returns.try_recv(), Err(mpsc::TryRecvError::Empty)));
    schedule.release((Actor::Background(1), Point::AfterCapture));
    for waiter in waiters {
        waiter.join().unwrap();
    }
    let mut returned: Vec<_> = returns.try_iter().collect();
    returned.sort();
    assert_eq!(returned, [1, 2]);
    assert!(newer_nodes(dir.path()) > 0);
    eprintln!("two public waiters: both observed pending2 then pending1, both returned after actual final write");
}

const CHILD_ROOT: &str = "HARN_PERSIST_EXIT_TEST_ROOT";

#[test]
fn public_restore_exit_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let cap = CodeIndexCapability::new();
    let mut schedule = Schedule::new(&root, &cap, &[(Actor::Background(0), Point::AfterCapture)]);
    assert!(cap.restore_from_disk(&root).unwrap());
    schedule.reached((Actor::Background(0), Point::AfterCapture));
    let waiter = harn_vm::runtime_stack::builder()
        .name("9771-waiter-1".into())
        .spawn(move || cap.wait_until_idle())
        .unwrap();
    schedule.reached((Actor::Waiter(1), Point::Waiting(1)));
    println!("9771 exit boundary: waiter reached pending1 before write");
    std::io::stdout().flush().unwrap();
    let mut permission = String::new();
    std::io::stdin().read_line(&mut permission).unwrap();
    assert_eq!(permission.trim(), "finish");
    schedule.release((Actor::Background(0), Point::AfterCapture));
    waiter.join().unwrap();
    assert!(newer_nodes(&root) > 0);
    println!("9771 exit boundary: persisted before child exit");
}

#[test]
fn public_restore_waits_for_durable_snapshot_before_process_exit() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, _) = seed(2);
    write(dir.path(), 0, "newer_marker");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "code_index::persistence_tests::public_restore_exit_child",
            "--nocapture",
        ])
        .env(CHILD_ROOT, dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while !lines
        .next()
        .expect("child waiter arrival")
        .unwrap()
        .contains("9771 exit boundary: waiter reached pending1")
    {}
    assert!(
        child.try_wait().unwrap().is_none(),
        "child still owns the pending write"
    );
    child.stdin.take().unwrap().write_all(b"finish\n").unwrap();
    let output: Vec<_> = lines.map(Result::unwrap).collect();
    assert!(child.wait().unwrap().success());
    assert!(output
        .iter()
        .any(|line| line.contains("persisted before child exit")));
    assert!(
        newer_nodes(dir.path()) > 0,
        "next process observes the final snapshot"
    );
    eprintln!("actual child exit: positive blocked waiter, released real write, terminal child and durable newer snapshot");
}
