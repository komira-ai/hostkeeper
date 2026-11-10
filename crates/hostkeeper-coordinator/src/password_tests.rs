//! The operator's passwords: the PHC string, verify and rehash, the dummy verify, the policy,
//! the cap on hashes in flight.

use std::num::NonZeroUsize;

use argon2::password_hash::PasswordHasher;
use argon2::{Algorithm, Argon2, Params, Version};

use crate::password::{
    HashSlots, MAX_PASSWORD_BYTES, PasswordError, PasswordSettings, Passwords, PolicyError,
    StoredHash, Verdict,
};

/// The least the algorithm takes, so a test hashes in microseconds.
fn cheap() -> PasswordSettings {
    PasswordSettings {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
        min_length: 12,
    }
}

fn passwords() -> Passwords {
    Passwords::new(cheap()).unwrap()
}

/// A hash of `password` as an earlier release with `settings` would have stored it.
fn hash_under(settings: PasswordSettings, algorithm: Algorithm, password: &str) -> StoredHash {
    let params = Params::new(
        settings.memory_kib,
        settings.iterations,
        settings.parallelism,
        None,
    )
    .unwrap();
    let phc = Argon2::new(algorithm, Version::V0x13, params)
        .hash_password(password.as_bytes())
        .unwrap();
    StoredHash::from(phc.to_string())
}

#[test]
fn a_hash_is_an_argon2id_phc_string_that_verifies_the_password() {
    let passwords = passwords();
    let stored = passwords.hash("correct horse battery").unwrap();
    assert!(stored.as_str().starts_with("$argon2id$v=19$m=8,t=1,p=1$"));
    assert_eq!(
        passwords.verify("correct horse battery", &stored),
        Verdict::Right { rehash: None }
    );
}

#[test]
fn a_wrong_password_is_wrong() {
    let passwords = passwords();
    let stored = passwords.hash("correct horse battery").unwrap();
    assert_eq!(
        passwords.verify("correct horse batter", &stored),
        Verdict::Wrong
    );
    assert_eq!(passwords.verify("", &stored), Verdict::Wrong);
}

#[test]
fn two_hashes_of_one_password_differ_and_both_verify() {
    let passwords = passwords();
    let a = passwords.hash("correct horse battery").unwrap();
    let b = passwords.hash("correct horse battery").unwrap();
    assert_ne!(a, b);
    assert!(matches!(
        passwords.verify("correct horse battery", &a),
        Verdict::Right { .. }
    ));
    assert!(matches!(
        passwords.verify("correct horse battery", &b),
        Verdict::Right { .. }
    ));
}

#[test]
fn a_stored_string_that_is_not_a_hash_is_wrong() {
    let passwords = passwords();
    assert_eq!(
        passwords.verify(
            "anything at all",
            &StoredHash::from("not a hash".to_owned())
        ),
        Verdict::Wrong
    );
}

#[test]
fn the_default_settings_are_the_owasp_minimum() {
    assert_eq!(
        PasswordSettings::default(),
        PasswordSettings {
            memory_kib: 19456,
            iterations: 2,
            parallelism: 1,
            min_length: 12
        }
    );
}

#[test]
fn settings_outside_the_algorithms_range_are_refused() {
    let err = Passwords::new(PasswordSettings {
        memory_kib: 1,
        ..cheap()
    })
    .unwrap_err();
    assert!(matches!(err, PasswordError::Params(_)));
    assert_eq!(
        err.to_string(),
        "the password hash parameters are refused: memory cost is too small"
    );
}

#[test]
fn a_hash_with_weaker_parameters_is_replaced_at_a_right_password() {
    let old = hash_under(cheap(), Algorithm::Argon2id, "correct horse battery");
    let stronger = Passwords::new(PasswordSettings {
        memory_kib: 16,
        ..cheap()
    })
    .unwrap();
    let Verdict::Right { rehash: Some(new) } = stronger.verify("correct horse battery", &old)
    else {
        panic!("a weaker hash must be replaced");
    };
    assert!(new.as_str().starts_with("$argon2id$v=19$m=16,t=1,p=1$"));
    // The new hash is current: the next login does not replace it again.
    assert_eq!(
        stronger.verify("correct horse battery", &new),
        Verdict::Right { rehash: None }
    );
}

#[test]
fn each_parameter_that_grew_makes_a_rehash() {
    let base = PasswordSettings {
        memory_kib: 16,
        ..cheap()
    };
    let old = hash_under(base, Algorithm::Argon2id, "correct horse battery");
    for grown in [
        PasswordSettings {
            memory_kib: 32,
            ..base
        },
        PasswordSettings {
            iterations: 2,
            ..base
        },
        PasswordSettings {
            parallelism: 2,
            ..base
        },
    ] {
        let verdict = Passwords::new(grown)
            .unwrap()
            .verify("correct horse battery", &old);
        assert!(
            matches!(verdict, Verdict::Right { rehash: Some(_) }),
            "{grown:?}"
        );
    }
}

#[test]
fn a_wrong_password_never_makes_a_rehash() {
    let old = hash_under(cheap(), Algorithm::Argon2id, "correct horse battery");
    let stronger = Passwords::new(PasswordSettings {
        iterations: 2,
        ..cheap()
    })
    .unwrap();
    assert_eq!(stronger.verify("not the password", &old), Verdict::Wrong);
}

#[test]
fn a_hash_stronger_than_the_settings_is_kept() {
    let strong = hash_under(
        PasswordSettings {
            memory_kib: 16,
            iterations: 2,
            parallelism: 2,
            min_length: 12,
        },
        Algorithm::Argon2id,
        "correct horse battery",
    );
    assert_eq!(
        passwords().verify("correct horse battery", &strong),
        Verdict::Right { rehash: None }
    );
}

#[test]
fn a_hash_of_another_argon2_variant_verifies_and_is_replaced_by_argon2id() {
    let old = hash_under(cheap(), Algorithm::Argon2i, "correct horse battery");
    assert!(old.as_str().starts_with("$argon2i$"));
    let Verdict::Right { rehash: Some(new) } = passwords().verify("correct horse battery", &old)
    else {
        panic!("a non-argon2id hash must be replaced");
    };
    assert!(new.as_str().starts_with("$argon2id$"));
}

#[test]
fn an_old_argon2_version_is_replaced() {
    let params = Params::new(8, 1, 1, None).unwrap();
    let phc = Argon2::new(Algorithm::Argon2id, Version::V0x10, params)
        .hash_password(b"correct horse battery")
        .unwrap();
    let old = StoredHash::from(phc.to_string());
    assert!(old.as_str().starts_with("$argon2id$v=16$"));
    assert!(matches!(
        passwords().verify("correct horse battery", &old),
        Verdict::Right { rehash: Some(_) }
    ));
}

#[test]
fn the_same_text_typed_two_ways_is_one_password() {
    let passwords = passwords();
    // U+FB01 (the "fi" ligature) and "fi"; U+FF21.. (fullwidth) and ASCII.
    let stored = passwords
        .hash("\u{FB01}nd the \u{FF21}\u{FF22}\u{FF23}\u{FF24} key")
        .unwrap();
    assert!(matches!(
        passwords.verify("find the ABCD key", &stored),
        Verdict::Right { .. }
    ));
}

#[test]
fn verify_counts_each_verify_and_absent_spends_one() {
    let passwords = passwords();
    assert_eq!(passwords.verifies(), 0);
    let stored = passwords.hash("correct horse battery").unwrap();
    assert_eq!(passwords.verifies(), 0, "hashing is not a verify");
    let _ = passwords.verify("correct horse battery", &stored);
    assert_eq!(passwords.verifies(), 1);
    passwords.verify_absent("correct horse battery");
    assert_eq!(passwords.verifies(), 2);
}

#[test]
fn the_absent_verify_never_matches_even_the_longest_password() {
    let passwords = passwords();
    let longest = "x".repeat(MAX_PASSWORD_BYTES);
    assert_eq!(passwords.check_policy(&longest, "admin", None), Ok(()));
    assert_eq!(passwords.verify(&longest, &passwords.dummy), Verdict::Wrong);
}

#[test]
fn the_absent_verify_costs_the_parameters_of_a_real_one() {
    let passwords = passwords();
    assert!(
        passwords
            .dummy
            .as_str()
            .starts_with("$argon2id$v=19$m=8,t=1,p=1$")
    );
    assert_eq!(passwords.settings(), &cheap());
}

#[test]
fn the_policy_takes_12_characters_and_refuses_11() {
    let passwords = passwords();
    assert_eq!(
        passwords.check_policy("abcdefghijkl", "admin", None),
        Ok(())
    );
    assert_eq!(
        passwords.check_policy("abcdefghijk", "admin", None),
        Err(PolicyError::TooShort { min: 12 })
    );
}

#[test]
fn the_policy_counts_characters_not_bytes() {
    let passwords = passwords();
    // 12 characters of 2 bytes each.
    assert_eq!(
        passwords.check_policy(&"é".repeat(12), "admin", None),
        Ok(())
    );
    // 11 characters of 3 bytes each is 33 bytes but still short.
    assert_eq!(
        passwords.check_policy(&"€".repeat(11), "admin", None),
        Err(PolicyError::TooShort { min: 12 })
    );
}

#[test]
fn the_minimum_length_is_a_setting() {
    let passwords = Passwords::new(PasswordSettings {
        min_length: 4,
        ..cheap()
    })
    .unwrap();
    assert_eq!(passwords.check_policy("abcd", "admin", None), Ok(()));
    assert_eq!(
        passwords.check_policy("abc", "admin", None),
        Err(PolicyError::TooShort { min: 4 })
    );
}

#[test]
fn the_policy_takes_256_bytes_and_refuses_257() {
    let passwords = passwords();
    assert_eq!(
        passwords.check_policy(&"a".repeat(256), "admin", None),
        Ok(())
    );
    assert_eq!(
        passwords.check_policy(&"a".repeat(257), "admin", None),
        Err(PolicyError::TooLong)
    );
}

#[test]
fn the_policy_refuses_the_user_name_and_the_current_password() {
    let passwords = passwords();
    assert_eq!(
        passwords.check_policy("administrator-1", "administrator-1", None),
        Err(PolicyError::SameAsUserName)
    );
    assert_eq!(
        passwords.check_policy(
            "correct horse battery",
            "admin",
            Some("correct horse battery")
        ),
        Err(PolicyError::SameAsCurrent)
    );
    assert_eq!(
        passwords.check_policy("correct horse battery", "admin", Some("another password")),
        Ok(())
    );
}

#[test]
fn the_policy_compares_the_normalised_forms() {
    let passwords = passwords();
    // The fullwidth form of the user name is the user name.
    assert_eq!(
        passwords.check_policy(
            "\u{FF41}\u{FF44}\u{FF4D}\u{FF49}\u{FF4E}-operator",
            "admin-operator",
            None
        ),
        Err(PolicyError::SameAsUserName)
    );
}

#[test]
fn the_policy_texts_name_the_rule_and_never_a_password() {
    assert_eq!(
        PolicyError::TooShort { min: 12 }.to_string(),
        "the password must be at least 12 characters"
    );
    assert_eq!(
        PolicyError::TooLong.to_string(),
        "the password must be at most 256 bytes"
    );
    assert_eq!(
        PolicyError::SameAsUserName.to_string(),
        "the password must not be the user name"
    );
    assert_eq!(
        PolicyError::SameAsCurrent.to_string(),
        "the new password must differ from the current one"
    );
}

#[test]
fn debug_output_holds_no_hash() {
    let passwords = passwords();
    let stored = passwords.hash("correct horse battery").unwrap();
    assert_eq!(format!("{stored:?}"), "StoredHash(..)");
    assert_eq!(
        format!("{passwords:?}"),
        "Passwords { settings: PasswordSettings { memory_kib: 8, iterations: 1, parallelism: 1, min_length: 12 }, .. }"
    );
}

#[test]
fn the_cap_gives_out_its_slots_and_takes_them_back() {
    let slots = HashSlots::new(NonZeroUsize::new(2).unwrap());
    let first = slots.try_acquire().unwrap();
    let second = slots.try_acquire().unwrap();
    assert!(slots.try_acquire().is_none(), "a third hash must not start");
    drop(first);
    let third = slots.try_acquire().unwrap();
    assert!(slots.try_acquire().is_none());
    drop((second, third));
    assert!(slots.try_acquire().is_some());
}

#[test]
fn clones_of_the_cap_share_it() {
    let slots = HashSlots::new(NonZeroUsize::MIN);
    let other = slots.clone();
    let held = slots.try_acquire().unwrap();
    assert!(other.try_acquire().is_none());
    drop(held);
    assert!(other.try_acquire().is_some());
}
