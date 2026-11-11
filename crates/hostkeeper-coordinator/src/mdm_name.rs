//! The MDM name (`docs/design/coordinator-mdm.md`, section 4.9, step 3): the DNS name the Macs
//! enroll at. It is kept with the setup's settings under [`KEY`], which no setting of
//! [`crate::mdm_settings::SETTINGS`] has, so `PutSetting` cannot set it and `ListSettings` does
//! not list it.

/// The key the name is kept under in the settings store.
pub(crate) const KEY: &str = "mdm_name";

/// `text` as the name is kept, if it is one: surrounding space dropped, lower case, two or more
/// labels of letters, digits and `-` that neither start nor end with `-`, each 1 to 63 long, at
/// most 253 in all, and a last label that is not all digits (so no IP address).
pub(crate) fn normalize(text: &str) -> Option<String> {
    let name = text.trim().to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();
    let label_ok = |label: &&str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let last_is_number = labels
        .last()
        .is_some_and(|last| last.bytes().all(|b| b.is_ascii_digit()));
    (name.len() <= 253 && labels.len() >= 2 && labels.iter().all(label_ok) && !last_is_number)
        .then_some(name)
}

/// The enrollment URL of `name`.
pub(crate) fn enrollment_url(name: &str) -> String {
    format!("https://{name}/enroll")
}

#[cfg(test)]
#[path = "mdm_name_tests.rs"]
mod tests;
