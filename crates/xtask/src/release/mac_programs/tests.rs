use super::{MAC_PROGRAMS, MacProgram, TEAM_ID};

#[test]
fn each_program_is_found_by_its_binary_name_and_no_other_name_is() {
    // Catches: a lookup that answers a program for a name the table does not hold.
    for program in MAC_PROGRAMS {
        assert_eq!(MacProgram::named(program.name), Some(program));
    }
    assert_eq!(MacProgram::named("gh"), None);
    assert_eq!(MacProgram::named(""), None);
}

#[test]
fn the_requirement_names_the_identifier_and_the_team_and_no_hash() {
    // Catches: a requirement that pins a hash of one build (`cdhash`), so a profile naming it
    // would stop matching at the next release.
    let hostkeeper = MacProgram::named("hostkeeper").expect("hostkeeper is in the table");
    assert_eq!(
        hostkeeper.requirement(),
        "identifier \"dev.hostkeeper.hostkeeper\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = U7RTX4XD3X"
    );
    for program in MAC_PROGRAMS {
        let requirement = program.requirement();
        assert!(!requirement.contains("cdhash"), "{requirement}");
        assert!(
            requirement.ends_with(&format!("= {TEAM_ID}")),
            "{requirement}"
        );
    }
}

#[test]
fn identifiers_and_names_are_unique() {
    // Catches: two programs signed under one identifier, which one grant would then cover both.
    for (i, a) in MAC_PROGRAMS.iter().enumerate() {
        for b in &MAC_PROGRAMS[i + 1..] {
            assert_ne!(a.name, b.name);
            assert_ne!(a.identifier, b.identifier);
        }
    }
}
