//! Tests for blob keys, publication locking and the local store.

use tempfile::tempdir;

use super::*;

fn new_store() -> (tempfile::TempDir, LocalBlobStore) {
    let temporary = tempdir().expect("create temporary directory");
    let store =
        LocalBlobStore::new(temporary.path().join("objects")).expect("create local blob store");
    (temporary, store)
}

#[test]
fn blob_key_parser_accepts_only_the_canonical_layout() {
    let digest = "a".repeat(DIGEST_HEX_LEN);
    let canonical = format!("blake3/aa/{digest}");
    assert_eq!(BlobKey::parse(&canonical).unwrap().as_str(), canonical);

    for invalid in [
        digest.clone(),
        format!("/blake3/aa/{digest}"),
        format!("blake3/../{digest}"),
        format!("blake3\\aa\\{digest}"),
        format!("blake3/ab/{digest}"),
        format!("blake3/aa/{}", "A".repeat(DIGEST_HEX_LEN)),
        format!("blake3/aa/{digest}/extra"),
        "C:/outside/blob".to_string(),
    ] {
        assert!(BlobKey::parse(&invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn local_store_rejects_relative_file_and_link_roots() {
    assert!(LocalBlobStore::new("relative/blob-root").is_err());

    let temporary = tempdir().unwrap();
    let file = temporary.path().join("not-a-directory");
    fs::write(&file, b"data").unwrap();
    assert!(LocalBlobStore::new(&file).is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = temporary.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temporary.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(LocalBlobStore::new(link).is_err());
    }
}

#[test]
fn publication_lock_is_shared_by_independent_handles_for_one_root() {
    let (temporary, first_store) = new_store();
    let second_store = LocalBlobStore::new(temporary.path().join("objects")).unwrap();
    let first = BlobPublicationLock::for_store(&first_store).unwrap();
    let second = BlobPublicationLock::for_store(&second_store).unwrap();

    assert!(Arc::ptr_eq(&first.gate, &second.gate));
}

#[tokio::test]
async fn trait_object_put_get_exists_and_delete_round_trip() {
    let (temporary, local) = new_store();
    let expected_root = fs::canonicalize(temporary.path().join("objects")).unwrap();
    assert_eq!(fs::canonicalize(local.root()).unwrap(), expected_root);

    let store: Arc<dyn BlobStore> = Arc::new(local);
    let bytes = b"content-addressed object";
    let key = store.put(bytes).await.unwrap();
    assert_eq!(key, BlobKey::from_bytes(bytes));
    assert!(store.exists(&key).await.unwrap());
    assert_eq!(store.get(&key).await.unwrap(), bytes);

    store.delete(&key).await.unwrap();
    store.delete(&key).await.unwrap();
    assert!(!store.exists(&key).await.unwrap());
}

#[tokio::test]
async fn duplicate_put_is_atomic_and_idempotent() {
    let (_temporary, store) = new_store();
    let bytes = b"same immutable bytes";
    let (first, second) = tokio::join!(store.put(bytes), store.put(bytes));
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first, second);
    assert_eq!(store.get(&first).await.unwrap(), bytes);
    assert!(store.list_orphans([&first]).await.unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_put_does_not_overwrite_corrupt_existing_data() {
    let (_temporary, store) = new_store();
    let original = b"original";
    let key = store.put(original).await.unwrap();
    let physical = store
        .backend
        .path_to_filesystem(&key.object_path().unwrap())
        .unwrap();
    tokio::fs::write(&physical, b"corrupt").await.unwrap();

    let error = store.put(original).await.unwrap_err().to_string();
    assert!(error.contains("无法验证"), "unexpected error: {error}");
    assert_eq!(tokio::fs::read(physical).await.unwrap(), b"corrupt");
}

#[tokio::test]
async fn range_reads_are_strict_and_half_open() {
    let (_temporary, store) = new_store();
    let key = store.put(b"0123456789").await.unwrap();

    assert_eq!(store.get_range(&key, 2..7).await.unwrap(), b"23456");
    assert!(store.get_range(&key, 3..3).await.is_err());
    assert!(
        store
            .get_range(&key, std::ops::Range { start: 7, end: 3 })
            .await
            .is_err()
    );
    assert!(store.get_range(&key, 9..11).await.is_err());
    assert!(store.get_range(&key, 10..11).await.is_err());
}

#[tokio::test]
async fn orphan_helpers_keep_referenced_and_unknown_files() {
    let (_temporary, store) = new_store();
    let retained = store.put(b"retained").await.unwrap();
    let orphan_a = store.put(b"orphan-a").await.unwrap();
    let orphan_b = store.put(b"orphan-b").await.unwrap();

    let unknown_directory = store.root().join(HASH_ALGORITHM).join("zz");
    tokio::fs::create_dir_all(&unknown_directory).await.unwrap();
    let unknown_file = unknown_directory.join("not-an-object");
    tokio::fs::write(&unknown_file, b"leave me alone")
        .await
        .unwrap();

    let mut expected = vec![orphan_a.clone(), orphan_b.clone()];
    expected.sort();
    assert_eq!(store.list_orphans([&retained]).await.unwrap(), expected);
    assert_eq!(store.delete_orphans([&retained]).await.unwrap(), expected);

    assert!(store.exists(&retained).await.unwrap());
    assert!(!store.exists(&orphan_a).await.unwrap());
    assert!(!store.exists(&orphan_b).await.unwrap());
    assert_eq!(
        tokio::fs::read(unknown_file).await.unwrap(),
        b"leave me alone"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn managed_namespace_symlink_is_rejected_without_following_it() {
    use std::os::unix::fs::symlink;

    let temporary = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let store = LocalBlobStore::new(temporary.path().join("objects")).unwrap();
    symlink(outside.path(), store.root().join(HASH_ALGORITHM)).unwrap();

    let error = store
        .put(b"must stay inside")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("符号链接"), "unexpected error: {error}");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
