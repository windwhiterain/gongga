//! What a device-wide reserve promises: one claim, one writer, one file.

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

/// # Invariant
///
/// Deleting is the claim: of two servers that see the same full disk, exactly one
/// spends it, and the loser learns that from the file being gone rather than from a
/// read that another process could interleave with.
#[test]
fn two_servers_share_one_claim_and_only_one_wins() {
    let dir = scratch("claim");
    let reserve = Reserve::new(dir.join("reserve"), 4096);
    // Often, because a race that is lost once in four rounds is a race that will be
    // lost on somebody's machine.
    for round in 0..16 {
        reserve.arm().expect("armed");
        let other = reserve.clone();
        let second = std::thread::spawn(move || other.claim());
        let first = reserve.claim();
        let second = second.join().expect("the thread");

        let wins = [&first, &second]
            .iter()
            .filter(|c| ***c == Claim::Won)
            .count();
        assert_eq!(
            wins, 1,
            "round {round}: exactly one caller spends the reserve"
        );
        assert!(
            [&first, &second].contains(&&Claim::Lost),
            "round {round}: the other one is told the file was gone: {first:?} {second:?}"
        );
        assert!(!reserve.armed(), "round {round}: the space is back");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// # Invariant
///
/// Arming is `create_new`, so the file a second server finds is the first one's:
/// calling it again keeps the space, and a file a killed writer left short is grown
/// to what the machine declared.
#[test]
fn arming_twice_keeps_the_space_and_repairs_a_short_file() {
    let dir = scratch("arm");
    let path = dir.join("reserve");
    let reserve = Reserve::new(&path, 8192);
    reserve.arm().expect("armed");
    reserve.arm().expect("arming an armed file is not an error");
    assert_eq!(
        std::fs::metadata(&path).expect("the file").len(),
        8192,
        "the declared size, not the size somebody else left"
    );

    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open")
        .set_len(1024)
        .expect("shorten");
    reserve.arm().expect("grown");
    assert_eq!(std::fs::metadata(&path).expect("the file").len(), 8192);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The words a full disk is reported with, in English and in this machine's own.
#[test]
fn the_usual_ways_a_full_disk_says_so_are_recognised() {
    for text in [
        "bash: write error: No space left on device",
        "There is not enough space on the disk.",
        "OSError: [Errno 28] No space left on device",
        "rusqlite: database or disk is full",
        "磁盘空间不足。",
    ] {
        assert!(disk_full(text), "{text}");
    }
    assert!(!disk_full("error: couldn't read src/main.rs"));
    assert!(!disk_full(""));
}

/// A report keeps what a reader can act on, and never splits a character.
#[test]
fn a_long_failure_is_cut_at_a_character_boundary() {
    let kept = head(&"磁".repeat(2_500));
    assert!(kept.ends_with("[...]"));
    assert_eq!(kept.chars().filter(|c| *c == '磁').count(), 2_000);
    assert_eq!(head("short"), "short");
}

#[test]
fn byte_counts_are_written_the_way_a_reader_reads_them() {
    assert_eq!(human(0), "0 B");
    assert_eq!(human(512), "512 B");
    assert_eq!(human(1024 * 1024), "1.0 MiB");
    assert_eq!(human(1024 * 1024 * 1024), "1.0 GiB");
}

/// # Invariant
///
/// Arming takes the space: a file that only claims a size would free nothing on
/// the day it is spent, and the report that needs the room would be the first
/// write to fail.
#[test]
fn the_space_a_reserve_holds_is_actually_taken() {
    let dir = scratch("space");
    let before = free_space(&dir).expect("this machine reports free space");
    let reserve = Reserve::new(dir.join("reserve"), 64 * 1024 * 1024);
    reserve.arm().expect("armed");
    let after = free_space(&dir).expect("this machine reports free space");
    assert!(
        before.saturating_sub(after) >= 60 * 1024 * 1024,
        "free space before {before}, after {after}"
    );
    reserve.claim();
    let back = free_space(&dir).expect("this machine reports free space");
    assert!(
        back >= after + 60 * 1024 * 1024,
        "spending it gives the room back: before {before}, after {after}, spent {back}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reserve this process states is the one a tool failure reads, and a test's
/// own file is not carried into the next test.
#[test]
fn the_installed_reserve_is_one_per_process_and_can_be_taken_away() {
    let dir = scratch("install");
    let reserve = Reserve::new(dir.join("reserve"), 1024);
    install(reserve.clone(), &dir);
    assert_eq!(installed().map(|w| w.reserve), Some(reserve.clone()));
    uninstall();
    assert_eq!(installed(), None);
    let _ = std::fs::remove_dir_all(&dir);
}
