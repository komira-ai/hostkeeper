//! A closed set of roles.
pub enum HostRole {
    Frontend,
    Storage,
}

/// A lifetime is not a static item.
pub fn named(text: &'static str) -> RoleName {
    RoleName::new(text)
}
