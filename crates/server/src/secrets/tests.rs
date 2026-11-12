use super::*;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_sealed_value_opens_to_what_was_sealed() {
    let keyring = Keyring::generate().unwrap();
    let sealed = keyring
        .seal(Column::NotificationChannels, "hunter2")
        .unwrap();
    assert!(sealed.starts_with(&format!("mfs1:{}:", keyring.ids()[0])));
    assert!(!sealed.contains("hunter2"));
    let opened = keyring.open(Column::NotificationChannels, &sealed).unwrap();
    assert_eq!(opened.expose(), "hunter2");
}

#[test]
fn sealing_twice_gives_two_values() {
    let keyring = Keyring::generate().unwrap();
    let a = keyring.seal(Column::AiApiKey, "sk").unwrap();
    let b = keyring.seal(Column::AiApiKey, "sk").unwrap();
    assert_ne!(a, b);
}

#[test]
fn a_value_moved_to_another_column_does_not_open() {
    let keyring = Keyring::generate().unwrap();
    let sealed = keyring.seal(Column::UserContacts, "url").unwrap();
    assert!(matches!(
        keyring.open(Column::IntegrationEndpoints, &sealed),
        Err(SecretsError::Corrupted)
    ));
}

#[test]
fn a_changed_value_does_not_open() {
    let keyring = Keyring::generate().unwrap();
    let sealed = keyring.seal(Column::AiApiKey, "sk-ant-1").unwrap();
    let mut bytes = sealed.into_bytes();
    let last = bytes.len() - 2;
    bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
    let changed = String::from_utf8(bytes).unwrap();
    assert!(matches!(
        keyring.open(Column::AiApiKey, &changed),
        Err(SecretsError::Corrupted)
    ));
    assert!(matches!(
        keyring.open(Column::AiApiKey, "mfs1:00000000:"),
        Err(SecretsError::UnknownKey(_))
    ));
    let id = keyring.ids()[0].to_owned();
    assert!(matches!(
        keyring.open(Column::AiApiKey, &format!("mfs1:{id}:AAAA")),
        Err(SecretsError::Corrupted)
    ));
}

#[test]
fn plaintext_and_other_keys_are_refused() {
    let keyring = Keyring::generate().unwrap();
    let other = Keyring::generate().unwrap();
    assert!(matches!(
        keyring.open(Column::AiApiKey, "sk-ant-plain"),
        Err(SecretsError::NotSealed)
    ));
    let sealed = other.seal(Column::AiApiKey, "sk").unwrap();
    match keyring.open(Column::AiApiKey, &sealed) {
        Err(SecretsError::UnknownKey(id)) => assert_eq!(id, other.ids()[0]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_first_key_seals_and_the_others_open() {
    let (old, new) = (
        Keyring::generate_text().unwrap(),
        Keyring::generate_text().unwrap(),
    );
    let old_ring = Keyring::parse(&old).unwrap();
    let sealed_before = old_ring.seal(Column::UserContacts, "before").unwrap();
    let both = Keyring::parse(&format!("{new}{old}")).unwrap();
    assert_eq!(both.ids().len(), 2);
    assert_eq!(
        both.open(Column::UserContacts, &sealed_before)
            .unwrap()
            .expose(),
        "before"
    );
    let sealed_after = both.seal(Column::UserContacts, "after").unwrap();
    assert!(sealed_after.contains(both.ids()[0]));
    assert!(old_ring.open(Column::UserContacts, &sealed_after).is_err());
}

#[test]
fn bad_key_files_are_refused() {
    assert!(Keyring::parse("# only a comment\n\n").is_err());
    assert!(Keyring::parse("not base64!").is_err());
    assert!(Keyring::parse(&STANDARD.encode([1u8; 16])).is_err());
    let line = STANDARD.encode([7u8; KEY_LEN]);
    assert!(Keyring::parse(&format!("{line}\n{line}\n")).is_err());
    assert!(Keyring::parse(&format!("  {line}  \n")).is_ok());
}

#[test]
fn the_key_file_is_created_once_and_private() {
    let data = tempfile::tempdir().unwrap();
    let first = Keyring::open_or_create(data.path(), true).unwrap();
    let path = data.path().join(DIR).join(KEYS_FILE);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&data.path().join(DIR)), 0o700);
    let again = Keyring::open_or_create(data.path(), true).unwrap();
    assert_eq!(first.ids(), again.ids());
    let sealed = first.seal(Column::AiApiKey, "sk").unwrap();
    assert_eq!(
        again.open(Column::AiApiKey, &sealed).unwrap().expose(),
        "sk"
    );
    assert!(!data.path().join(DIR).join(".keys.tmp").exists());
}

#[test]
fn an_added_server_never_makes_a_key() {
    let data = tempfile::tempdir().unwrap();
    let error = Keyring::open_or_create(data.path(), false).unwrap_err();
    assert!(matches!(error, SecretsError::Missing { .. }), "{error}");
    assert!(error.to_string().contains("--add-server"));
    assert!(!data.path().join(DIR).exists());
}

#[test]
fn a_key_file_others_can_read_is_refused() {
    let data = tempfile::tempdir().unwrap();
    Keyring::open_or_create(data.path(), true).unwrap();
    let path = data.path().join(DIR).join(KEYS_FILE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Keyring::open_or_create(data.path(), true),
        Err(SecretsError::Permissions { mode: 0o644, .. })
    ));
}

#[test]
fn the_keyring_never_prints_its_keys() {
    let text = Keyring::generate_text().unwrap();
    let keyring = Keyring::parse(&text).unwrap();
    let line = text.lines().last().unwrap();
    let shown = format!("{keyring:?}");
    assert!(!shown.contains(line), "{shown}");
    assert!(shown.contains(keyring.ids()[0]));
}
