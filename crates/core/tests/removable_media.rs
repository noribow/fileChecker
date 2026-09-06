//! `repo::find_or_create_removable_media` behavior (`docs/requirements.md` §10.4/§6/
//! §10.21): reconnecting the same identified medium reuses its `removable_media` row
//! (so past scan history stays reachable, §6); a different identifier is a different
//! medium; and §10.21's manual-label fallback (`identifier_type = 'user_defined'`)
//! goes through the exact same matching, since it's stored the same way.

use filechecker_core::db::{open_in_memory, repo};

fn now() -> i64 {
    1_700_000_000_000
}

#[test]
fn reconnecting_the_same_identifier_reuses_the_same_media_row() {
    let conn = open_in_memory().unwrap();

    let first = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "device_serial",
        "USB-SERIAL-1",
        Some("My USB Drive"),
        now(),
    )
    .unwrap();

    // Reconnected later: same platform/type/value -> the same row, with last_seen_at
    // bumped rather than a duplicate row created.
    let second = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "device_serial",
        "USB-SERIAL-1",
        None,
        now() + 1000,
    )
    .unwrap();
    assert_eq!(first, second);

    let row = repo::get_removable_media(&conn, first).unwrap().unwrap();
    assert_eq!(row.last_seen_at, now() + 1000);
    // A later reconnect with no display name doesn't blank out the one already known.
    assert_eq!(row.display_name.as_deref(), Some("My USB Drive"));

    let all = repo::list_removable_media(&conn).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn a_different_identifier_is_a_different_medium() {
    let conn = open_in_memory().unwrap();

    let a = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "device_serial",
        "SERIAL-A",
        None,
        now(),
    )
    .unwrap();
    let b = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "device_serial",
        "SERIAL-B",
        None,
        now(),
    )
    .unwrap();
    assert_ne!(a, b);
    assert_eq!(repo::list_removable_media(&conn).unwrap().len(), 2);
}

#[test]
fn the_user_defined_fallback_label_reuses_the_same_unique_constraint() {
    let conn = open_in_memory().unwrap();

    // §10.21: auto-identification failed, so the CLI/GUI records a user-typed label
    // instead — same repo call, just a different identifier_type.
    let first = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "user_defined",
        "grandma's photo drive",
        None,
        now(),
    )
    .unwrap();
    let second = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "user_defined",
        "grandma's photo drive",
        None,
        now() + 1000,
    )
    .unwrap();
    assert_eq!(
        first, second,
        "the same label reconnects to the same medium"
    );

    // A typo'd or different label is, correctly, a different medium — the system has
    // no way to know it's physically the same drive (§10.21's documented limitation).
    let typo = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "user_defined",
        "grandmas photo drive",
        None,
        now(),
    )
    .unwrap();
    assert_ne!(first, typo);
}
