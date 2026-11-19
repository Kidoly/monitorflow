use super::*;

const VENDOR: &str = r#"
-- A made-up vendor MIB for the tests.
TEST-NAS-MIB DEFINITIONS ::= BEGIN

IMPORTS
    MODULE-IDENTITY, OBJECT-TYPE, Integer32, Counter64, enterprises
        FROM SNMPv2-SMI
    TEXTUAL-CONVENTION, DisplayString, TruthValue
        FROM SNMPv2-TC;

testNas MODULE-IDENTITY
    LAST-UPDATED "202611190000Z"
    ORGANIZATION "Test"
    CONTACT-INFO "none"
    DESCRIPTION  "The   test
                  NAS. -- not a comment inside text"
    ::= { enterprises 99999 }

RaidState ::= TEXTUAL-CONVENTION
    STATUS      current
    DESCRIPTION "A volume's state."
    SYNTAX      INTEGER { normal(1), repairing(2), degraded(11), crashed(12) }

system  OBJECT IDENTIFIER ::= { testNas 1 }
storage OBJECT IDENTIFIER ::= { testNas 3 }

systemStatus OBJECT-TYPE
    SYNTAX      INTEGER { normal(1), failed(2) }
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "Whether the system is well."
    ::= { system 1 }

temperature OBJECT-TYPE
    SYNTAX      Integer32 (-50..150)
    UNITS       "tenths of a degree"
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "Temperature."
    ::= { system 2 }

fanOk OBJECT-TYPE
    SYNTAX      TruthValue
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "Fans."
    ::= { system 3 }

raidTable OBJECT-TYPE
    SYNTAX      SEQUENCE OF RaidEntry
    MAX-ACCESS  not-accessible
    STATUS      current
    DESCRIPTION "Volumes."
    ::= { storage 1 }

raidEntry OBJECT-TYPE
    SYNTAX      RaidEntry
    MAX-ACCESS  not-accessible
    STATUS      current
    DESCRIPTION "A volume."
    INDEX       { raidIndex }
    ::= { raidTable 1 }

RaidEntry ::= SEQUENCE {
    raidIndex   Integer32,
    raidName    DisplayString,
    raidStatus  RaidState,
    raidBytes   Counter64
}

raidIndex OBJECT-TYPE
    SYNTAX      Integer32 (0..255)
    MAX-ACCESS  not-accessible
    STATUS      current
    DESCRIPTION "Index."
    ::= { raidEntry 1 }

raidName OBJECT-TYPE
    SYNTAX      DisplayString (SIZE (0..64))
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "Name."
    ::= { raidEntry 2 }

raidStatus OBJECT-TYPE
    SYNTAX      RaidState
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "State."
    DEFVAL      { normal }
    ::= { raidEntry 3 }

raidBytes OBJECT-TYPE
    SYNTAX      Counter64
    MAX-ACCESS  read-only
    STATUS      current
    DESCRIPTION "Bytes written."
    ::= { raidEntry 4 }

END
"#;

const CHILD: &str = r#"
TEST-NAS-EXTRA-MIB DEFINITIONS ::= BEGIN
IMPORTS OBJECT-TYPE FROM SNMPv2-SMI
        testNas FROM TEST-NAS-MIB;
extra OBJECT IDENTIFIER ::= { testNas 9 }
upsLink OBJECT-TYPE
    SYNTAX INTEGER { up(1), down(2) }
    ACCESS read-only
    STATUS mandatory
    DESCRIPTION "SMIv1 style."
    ::= { extra 1 }
upsTrap TRAP-TYPE
    ENTERPRISE extra
    VARIABLES { upsLink }
    ::= 7
END
"#;

fn file(name: &str, text: &str) -> (String, String) {
    (name.to_owned(), text.to_owned())
}

fn object<'a>(modules: &'a [Module], name: &str) -> &'a Object {
    modules
        .iter()
        .flat_map(|m| &m.objects)
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn a_vendor_mib_names_its_objects() {
    let modules = parse(&[file("nas.mib", VENDOR)], &Known::default()).unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "TEST-NAS-MIB");
    let root = object(&modules, "testNas");
    assert_eq!(root.oid, vec![1, 3, 6, 1, 4, 1, 99999]);
    assert_eq!(root.kind, ObjectKind::Node);
    assert_eq!(
        root.description,
        "The test NAS. -- not a comment inside text"
    );
    let status = object(&modules, "systemStatus");
    assert_eq!(status.oid, vec![1, 3, 6, 1, 4, 1, 99999, 1, 1]);
    assert_eq!(status.kind, ObjectKind::Scalar);
    assert_eq!(
        status.enumeration,
        vec![(1, "normal".into()), (2, "failed".into())]
    );
    assert_eq!(status.access.as_deref(), Some("read-only"));
    let temp = object(&modules, "temperature");
    assert_eq!(temp.units.as_deref(), Some("tenths of a degree"));
    assert_eq!(temp.syntax.as_deref(), Some("Integer32"));
    // A textual convention imported from the base gives its values.
    assert_eq!(object(&modules, "fanOk").enumeration[0], (1, "true".into()));
    assert_eq!(object(&modules, "raidTable").kind, ObjectKind::Table);
    assert_eq!(object(&modules, "raidEntry").kind, ObjectKind::Entry);
    let raid = object(&modules, "raidStatus");
    assert_eq!(raid.kind, ObjectKind::Column);
    assert_eq!(raid.oid, vec![1, 3, 6, 1, 4, 1, 99999, 3, 1, 1, 3]);
    // A local textual convention gives its values too.
    assert_eq!(raid.enumeration.len(), 4);
    assert_eq!(raid.enumeration[3], (12, "crashed".into()));
    assert_eq!(modules[0].types["RaidState"].enumeration.len(), 4);
    assert!(modules[0].types["RaidEntry"].sequence);
    use crate::custom::ValueKind;
    assert_eq!(suggest(object(&modules, "raidBytes")).0, ValueKind::Counter);
    assert_eq!(suggest(object(&modules, "raidName")).0, ValueKind::Text);
    assert_eq!(suggest(raid).0, ValueKind::Gauge);
    assert_eq!(
        suggest(temp).1,
        "tenths of a degr",
        "cut to a unit's length"
    );
}

#[test]
fn imports_resolve_across_files_and_known_modules() {
    // Together, in either order.
    let both = parse(
        &[file("extra.mib", CHILD), file("nas.mib", VENDOR)],
        &Known::default(),
    )
    .unwrap();
    let link = object(&both, "upsLink");
    assert_eq!(link.oid, vec![1, 3, 6, 1, 4, 1, 99999, 9, 1]);
    assert_eq!(link.access.as_deref(), Some("read-only"));
    assert!(both
        .iter()
        .flat_map(|m| &m.objects)
        .all(|o| o.name != "upsTrap"));
    // Alone, it names the module to upload.
    match parse(&[file("extra.mib", CHILD)], &Known::default()).unwrap_err() {
        MibError::Missing(missing) => {
            assert_eq!(missing.keys().collect::<Vec<_>>(), ["TEST-NAS-MIB"]);
            assert!(missing["TEST-NAS-MIB"].contains("testNas"));
        }
        other => panic!("{other:?}"),
    }
    // After, with what was uploaded before.
    let mut known = Known::default();
    for module in parse(&[file("nas.mib", VENDOR)], &known).unwrap() {
        known.add(&module);
    }
    let later = parse(&[file("extra.mib", CHILD)], &known).unwrap();
    assert_eq!(object(&later, "upsLink").oid.len(), 9);
}

#[test]
fn bad_files_are_refused_within_limits() {
    let none = Known::default();
    let err = |text: &str| {
        parse(&[file("x.mib", text)], &none)
            .unwrap_err()
            .to_string()
    };
    assert!(err("hello").contains("no MIB module"));
    assert!(err("A DEFINITIONS ::= BEGIN x OBJECT IDENTIFIER ::= { iso 3 ").contains("x.mib"));
    assert!(
        err("A DEFINITIONS ::= BEGIN x OBJECT IDENTIFIER ::= { iso 3 }").contains("never ends")
    );
    assert!(err("A DEFINITIONS ::= BEGIN d OBJECT-TYPE DESCRIPTION \"open").contains("never ends"));
    assert!(err(&format!(
        "A DEFINITIONS ::= BEGIN x OBJECT-TYPE DEFVAL {}{} ::= {{ iso 1 }} END",
        "{".repeat(40),
        "}".repeat(40)
    ))
    .contains("deeply"));
    assert!(err(&" ".repeat(MAX_FILE + 1)).contains("larger"));
    let many: Vec<(String, String)> = (0..=MAX_FILES).map(|i| file(&i.to_string(), "")).collect();
    assert!(parse(&many, &none)
        .unwrap_err()
        .to_string()
        .contains("at most"));
    let mut big = String::from("A DEFINITIONS ::= BEGIN\n");
    for i in 0..=MAX_OBJECTS {
        big.push_str(&format!("o{i} OBJECT IDENTIFIER ::= {{ iso {i} }}\n"));
    }
    big.push_str("END\n");
    assert!(err(&big).contains("more than"));
    // Nothing panics on arbitrary text.
    for junk in [
        "::= ::= {",
        "A DEFINITIONS ::= BEGIN }}}} END",
        "A DEFINITIONS ::= BEGIN x OBJECT-TYPE SYNTAX ::= { } END",
        "A DEFINITIONS ::= BEGIN T ::= INTEGER { a(",
        "'0A'H \"",
        "A DEFINITIONS ::= BEGIN OBJECT-TYPE MACRO ::= BEGIN",
    ] {
        let _ = parse(&[file("j.mib", junk)], &none);
    }
}

#[test]
fn the_smi_itself_reads() {
    // A macro definition and value assignments, as in SNMPv2-SMI.
    let smi = r#"
SMI-LIKE DEFINITIONS ::= BEGIN
org            OBJECT IDENTIFIER ::= { iso 3 }
dod            OBJECT IDENTIFIER ::= { org 6 }
internet       OBJECT IDENTIFIER ::= { iso org(3) dod(6) 1 }
OBJECT-TYPE MACRO ::=
BEGIN
    TYPE NOTATION ::= "SYNTAX" Syntax
    VALUE NOTATION ::= value(VALUE ObjectName)
END
ObjectName ::= OBJECT IDENTIFIER
maxValue INTEGER ::= 255
zero OBJECT IDENTIFIER ::= { 0 0 }
END
"#;
    let modules = parse(&[file("smi.mib", smi)], &Known::default()).unwrap();
    assert_eq!(object(&modules, "internet").oid, vec![1, 3, 6, 1]);
    assert_eq!(object(&modules, "dod").oid, vec![1, 3, 6]);
    assert_eq!(object(&modules, "zero").oid, vec![0, 0]);
}

#[test]
fn every_cut_of_a_file_reads_or_is_refused_without_panicking() {
    let none = Known::default();
    for (end, _) in VENDOR.char_indices() {
        let _ = parse(&[file("cut.mib", &VENDOR[..end])], &none);
        let _ = parse(&[file("tail.mib", &VENDOR[end..])], &none);
    }
}

#[test]
fn resolved_oids_are_bounded() {
    // Each object under the previous one: the OIDs grow by one arc each.
    let chain = |n: usize| {
        let mut text = String::from(
            "CHAIN-MIB DEFINITIONS ::= BEGIN\nIMPORTS enterprises FROM SNMPv2-SMI;\na0 OBJECT IDENTIFIER ::= { enterprises 1 }\n",
        );
        for i in 1..n {
            text.push_str(&format!("a{i} OBJECT IDENTIFIER ::= {{ a{} 1 }}\n", i - 1));
        }
        text.push_str("END\n");
        text
    };
    assert!(parse(&[file("short.mib", &chain(100))], &Known::default()).is_ok());
    let started = std::time::Instant::now();
    match parse(&[file("long.mib", &chain(20_000))], &Known::default()) {
        Err(MibError::Invalid { file, reason }) => {
            assert_eq!(file, "long.mib");
            assert!(reason.contains("arcs"), "{reason}");
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a chain of 20,000 objects was accepted"),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}
