//! What a threshold promises: read on every call, said once per episode.

use super::*;

/// A directory of this test's own, so two runs of the suite cannot meet.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gongga-disk-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The rows this test wrote, so an episode can be counted rather than assumed.
fn events(store: &crate::store::Store) -> Vec<crate::store::EventRow> {
    store.events_after(0, Some(EVENT), None, 100).expect("rows")
}

/// # Invariant
///
/// A disk that stays low says so once: the reading happens on every tool call, and
/// a call is not an episode. A disk that comes back above the threshold closes the
/// episode, so the next fall is news again.
///
/// The threshold is the test's own number, which is what makes this provable: a
/// hundred terabytes is below no disk, and one byte is below every one.
#[tokio::test]
async fn a_low_disk_is_reported_once_and_again_after_recovering() {
    let dir = scratch("episode");
    let store = crate::store::Store::open(&dir.join("gongga.db")).expect("a store");
    let huge = 100 * 1024 * 1024 * 1024 * 1024;
    let tiny = 1;
    let cwd = Path::new(".");

    install(huge, &dir);
    note_tool_end(&store, "work", "bash", cwd, true, None).await;
    assert_eq!(
        events(&store).len(),
        1,
        "a low disk reports on a call that worked"
    );
    for _ in 0..5 {
        note_tool_end(&store, "work", "bash", cwd, false, None).await;
    }
    assert_eq!(events(&store).len(), 1, "and not again while it stays low");

    install(tiny, &dir);
    note_tool_end(&store, "work", "bash", cwd, true, None).await;
    assert_eq!(events(&store).len(), 1, "an ordinary disk says nothing");

    install(huge, &dir);
    note_tool_end(&store, "work", "bash", cwd, true, None).await;
    assert_eq!(events(&store).len(), 2, "the next fall is a new episode");

    // A peer's disk is its own episode, and its row names the machine: one full
    // disk must not hide the other, and a report must know where to send a monitor.
    let peer = Some(super::Peer {
        machine: "build-box".to_string(),
        volumes: vec![super::Volume {
            label: "build-box".to_string(),
            free: Some(0),
        }],
    });
    note_tool_end(&store, "work", "bash", cwd, true, peer.clone()).await;
    assert_eq!(events(&store).len(), 3, "the peer's disk is news of its own");
    let peer_row: serde_json::Value =
        serde_json::from_str(&events(&store)[2].payload).expect("a payload");
    assert_eq!(peer_row["machine"], "build-box");
    note_tool_end(&store, "work", "bash", cwd, true, peer).await;
    assert_eq!(events(&store).len(), 3, "and not again while it stays low");

    let payload: serde_json::Value =
        serde_json::from_str(&events(&store)[1].payload).expect("a payload");
    assert_eq!(payload["session"], "work");
    assert_eq!(payload["tool"], "bash");
    assert_eq!(payload["tool_ok"], true);
    assert!(payload["volumes"].as_array().is_some_and(|v| !v.is_empty()));
    uninstall();
    let _ = std::fs::remove_dir_all(&dir);
}

/// # Invariant
///
/// Nothing is held back and nothing is written to be able to check: the room is
/// read where it is, one call per volume, and it moves when something big does.
#[test]
fn the_room_left_is_read_from_the_volume_itself() {
    let dir = scratch("space");
    let before = free_space(&dir).expect("this machine reports free space");
    assert!(before > 0);
    let big = dir.join("big");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&big)
        .expect("create")
        .set_len(64 * 1024 * 1024)
        .expect("reserve 64 MiB");
    let after = free_space(&dir).expect("this machine reports free space");
    assert!(
        before.saturating_sub(after) >= 60 * 1024 * 1024,
        "free space before {before}, after {after}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The volumes are what a report carries, so one of them has to be the home's.
#[test]
fn the_home_volume_is_among_the_volumes_a_report_carries() {
    let dir = scratch("volumes");
    let volumes = volumes(&dir);
    assert!(!volumes.is_empty(), "a machine with no volume has no home");
    assert!(
        volumes.iter().any(|volume| volume.free.is_some()),
        "at least one volume reports its room: {volumes:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The smallest volume decides, because a command writes where it says.
#[test]
fn the_volume_with_the_least_room_decides() {
    assert!(low(Some(0), 1024));
    assert!(low(Some(1023), 1024));
    assert!(!low(Some(1024), 1024));
    assert!(!low(None, 1024), "an unreadable reading is not a full disk");
}

#[test]
fn byte_counts_are_written_the_way_a_reader_reads_them() {
    assert_eq!(human(0), "0 B");
    assert_eq!(human(512), "512 B");
    assert_eq!(human(1024 * 1024), "1.0 MiB");
    assert_eq!(human(1024 * 1024 * 1024), "1.0 GiB");
}
